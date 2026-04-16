use anyhow::{anyhow, Context, Result};
use common::{default_capabilities_for_venue, PoolEdge, RoutePlan, RouteStep, VenueKind};
use ethers::types::{Address, U256};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::str::FromStr;

#[derive(Default, Clone)]
pub struct LiquidityGraph {
    adjacency: HashMap<Address, Vec<PoolEdge>>,
}

impl LiquidityGraph {
    pub fn add_edge(&mut self, edge: PoolEdge) {
        self.adjacency.entry(edge.token_in).or_default().push(edge);
    }

    pub fn add_bidirectional_edge(&mut self, edge: PoolEdge) {
        let reverse = PoolEdge {
            token_in: edge.token_out,
            token_out: edge.token_in,
            path: if edge.path.is_empty() {
                Vec::new()
            } else {
                let mut path = edge.path.clone();
                path.reverse();
                path
            },
            ..edge.clone()
        };
        self.add_edge(edge);
        self.add_edge(reverse);
    }

    pub fn plan_routes(
        &self,
        source: Address,
        target: Address,
        amount_in: U256,
        max_hops: usize,
        max_candidates: usize,
    ) -> Vec<RoutePlan> {
        let mut results = Vec::new();
        let mut visited = HashSet::from([source]);
        let mut steps = Vec::<RouteStep>::new();

        self.search(
            source,
            target,
            amount_in,
            amount_in,
            max_hops,
            &mut visited,
            &mut steps,
            &mut results,
        );

        results.sort_by(|left, right| {
            right
                .expected_amount_out
                .cmp(&left.expected_amount_out)
                .then_with(|| left.estimated_gas.cmp(&right.estimated_gas))
        });
        results.truncate(max_candidates);
        results
    }

    pub fn plan_cycles(
        &self,
        settlement_token: Address,
        amount_in: U256,
        max_hops: usize,
        max_candidates: usize,
    ) -> Vec<RoutePlan> {
        let mut results = Vec::new();
        let mut visited = HashSet::from([settlement_token]);
        let mut steps = Vec::<RouteStep>::new();

        self.search_cycles(
            settlement_token,
            settlement_token,
            amount_in,
            amount_in,
            max_hops,
            &mut visited,
            &mut steps,
            &mut results,
        );

        results.sort_by(|left, right| {
            right
                .expected_amount_out
                .cmp(&left.expected_amount_out)
                .then_with(|| left.estimated_gas.cmp(&right.estimated_gas))
        });
        results.truncate(max_candidates);
        results
    }

    fn search(
        &self,
        current: Address,
        target: Address,
        initial_amount: U256,
        amount: U256,
        remaining_hops: usize,
        visited: &mut HashSet<Address>,
        current_steps: &mut Vec<RouteStep>,
        results: &mut Vec<RoutePlan>,
    ) {
        if remaining_hops == 0 {
            return;
        }

        if let Some(edges) = self.adjacency.get(&current) {
            for edge in edges {
                if !edge.capabilities.plannable {
                    continue;
                }

                if visited.contains(&edge.token_out) {
                    continue;
                }

                let projected_amount = apply_step_fee(amount, edge.fee_bps);
                current_steps.push(RouteStep::from(edge));

                if edge.token_out == target {
                    let estimated_gas = current_steps
                        .iter()
                        .map(|step| step.estimated_gas)
                        .sum::<u64>();
                    results.push(RoutePlan {
                        source_token: current_steps.first().map(|step| step.token_in).unwrap_or(current),
                        target_token: target,
                        amount_in: initial_amount,
                        steps: current_steps.clone(),
                        expected_amount_out: projected_amount,
                        estimated_gas,
                    });
                } else {
                    visited.insert(edge.token_out);
                    self.search(
                        edge.token_out,
                        target,
                        initial_amount,
                        projected_amount,
                        remaining_hops - 1,
                        visited,
                        current_steps,
                        results,
                    );
                    visited.remove(&edge.token_out);
                }

                current_steps.pop();
            }
        }
    }

    fn search_cycles(
        &self,
        settlement_token: Address,
        current: Address,
        initial_amount: U256,
        amount: U256,
        remaining_hops: usize,
        visited: &mut HashSet<Address>,
        current_steps: &mut Vec<RouteStep>,
        results: &mut Vec<RoutePlan>,
    ) {
        if remaining_hops == 0 {
            return;
        }

        if let Some(edges) = self.adjacency.get(&current) {
            for edge in edges {
                if !edge.capabilities.plannable {
                    continue;
                }

                let closes_cycle = edge.token_out == settlement_token && !current_steps.is_empty();
                if visited.contains(&edge.token_out) && !closes_cycle {
                    continue;
                }

                let projected_amount = apply_step_fee(amount, edge.fee_bps);
                current_steps.push(RouteStep::from(edge));

                if closes_cycle {
                    let estimated_gas = current_steps
                        .iter()
                        .map(|step| step.estimated_gas)
                        .sum::<u64>();
                    results.push(RoutePlan {
                        source_token: settlement_token,
                        target_token: settlement_token,
                        amount_in: initial_amount,
                        steps: current_steps.clone(),
                        expected_amount_out: projected_amount,
                        estimated_gas,
                    });
                } else {
                    visited.insert(edge.token_out);
                    self.search_cycles(
                        settlement_token,
                        edge.token_out,
                        initial_amount,
                        projected_amount,
                        remaining_hops - 1,
                        visited,
                        current_steps,
                        results,
                    );
                    visited.remove(&edge.token_out);
                }

                current_steps.pop();
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedPoolEntry {
    pub name: String,
    pub venue: String,
    pub token_in: String,
    pub token_out: String,
    pub router: String,
    pub quoter: Option<String>,
    pub fee_bps: u32,
    #[serde(default)]
    pub stable: bool,
    pub estimated_gas: u64,
}

impl SeedPoolEntry {
    pub fn into_pool_edge(self) -> Result<PoolEdge> {
        Ok(PoolEdge {
            name: self.name,
            venue: parse_venue(&self.venue)?,
            capabilities: default_capabilities_for_venue(parse_venue(&self.venue)?),
            router: parse_address(&self.router)?,
            quoter: self.quoter.as_deref().map(parse_address).transpose()?,
            token_in: parse_address(&self.token_in)?,
            token_out: parse_address(&self.token_out)?,
            fee_bps: self.fee_bps,
            liquidity_score: 10_000,
            path: vec![parse_address(&self.token_in)?, parse_address(&self.token_out)?],
            stable: self.stable,
            pool_key: None,
            estimated_gas: self.estimated_gas,
        })
    }
}

pub fn load_seed_graph(path: &str) -> Result<LiquidityGraph> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read liquidity seed file at {path}"))?;
    let entries: Vec<SeedPoolEntry> = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse liquidity seed file at {path}"))?;

    let mut graph = LiquidityGraph::default();
    for entry in entries {
        graph.add_bidirectional_edge(entry.into_pool_edge()?);
    }

    Ok(graph)
}

fn apply_step_fee(amount: U256, fee_bps: u32) -> U256 {
    let numerator = U256::from(10_000u64.saturating_sub(fee_bps as u64));
    amount.saturating_mul(numerator) / U256::from(10_000u64)
}

fn parse_address(value: &str) -> Result<Address> {
    Address::from_str(value).map_err(|error| anyhow!("invalid address {value}: {error}"))
}

fn parse_venue(value: &str) -> Result<VenueKind> {
    match value {
        "uniswap_v2" => Ok(VenueKind::UniswapV2),
        "uniswap_v3" => Ok(VenueKind::UniswapV3),
        "uniswap_v4" => Ok(VenueKind::UniswapV4),
        "aerodrome_v2" => Ok(VenueKind::AerodromeV2),
        "virtuals" => Ok(VenueKind::Virtuals),
        _ => Err(anyhow!("unsupported venue kind in seed file: {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::VenueKind;

    fn address(value: u64) -> Address {
        Address::from_low_u64_be(value)
    }

    #[test]
    fn finds_two_hop_route() {
        let mut graph = LiquidityGraph::default();
        graph.add_edge(PoolEdge {
            name: "WETH-USDC".to_string(),
            venue: VenueKind::UniswapV3,
            capabilities: default_capabilities_for_venue(VenueKind::UniswapV3),
            router: address(10),
            quoter: Some(address(11)),
            token_in: address(1),
            token_out: address(2),
            fee_bps: 30,
            liquidity_score: 10_000,
            path: vec![address(1), address(2)],
            stable: false,
            pool_key: None,
            estimated_gas: 120_000,
        });
        graph.add_edge(PoolEdge {
            name: "USDC-TOKEN".to_string(),
            venue: VenueKind::AerodromeV2,
            capabilities: default_capabilities_for_venue(VenueKind::AerodromeV2),
            router: address(12),
            quoter: Some(address(13)),
            token_in: address(2),
            token_out: address(3),
            fee_bps: 30,
            liquidity_score: 9_000,
            path: vec![address(2), address(3)],
            stable: false,
            pool_key: None,
            estimated_gas: 140_000,
        });

        let routes = graph.plan_routes(address(1), address(3), U256::from(1_000_000u64), 3, 8);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].steps.len(), 2);
    }

    #[test]
    fn parses_seed_venue() {
        assert_eq!(parse_venue("uniswap_v3").unwrap(), VenueKind::UniswapV3);
    }
}
