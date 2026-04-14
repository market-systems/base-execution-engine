#![allow(deprecated)]

use anyhow::{anyhow, Result};
use common::{
    ContractCall, PoolKey, RouteStep, VenueKind, AERODROME_FACTORY, AERODROME_ROUTER,
    CLANKER_HOOK_DYNAMIC, CLANKER_HOOK_STATIC, PANCAKESWAP_V3_QUOTER, PANCAKESWAP_V3_ROUTER,
    UNIV3_QUOTER, UNIV3_ROUTER, UNIV4_QUOTER, UNIVERSAL_ROUTER, WETH_BASE,
};
use ethers::abi::{Abi, Function, Param, ParamType, StateMutability, Token as AbiToken, Tokenizable};
use ethers::prelude::*;
use std::sync::Arc;

pub trait ExecutionAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn venue(&self) -> VenueKind;
    fn encode_quote(&self, amount_in: U256, token_in: Address, token_out: Address) -> Result<ContractCall>;
    fn decode_quote(&self, output: Bytes) -> Result<U256>;
    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        token_out: Address,
        recipient: Address,
        amount_out_min: U256,
        deadline: U256,
    ) -> Result<ContractCall>;
}

pub fn adapter_for_step(step: &RouteStep) -> Result<Arc<dyn ExecutionAdapter>> {
    match step.venue {
        VenueKind::UniswapV3 => Ok(Arc::new(UniswapV3Adapter {
            name: step.name.clone(),
            router: step.router,
            quoter: step.quoter.unwrap_or(*UNIV3_QUOTER),
            fee: step.fee_bps,
        })),
        VenueKind::AerodromeV2 => Ok(Arc::new(AerodromeV2Adapter {
            name: step.name.clone(),
            router: step.router,
            factory: *AERODROME_FACTORY,
            path: if step.path.is_empty() {
                vec![step.token_in, step.token_out]
            } else {
                step.path.clone()
            },
            stable: step.stable,
        })),
        VenueKind::UniswapV4 => Ok(Arc::new(UniswapV4Adapter {
            name: step.name.clone(),
            pool_key: step
                .pool_key
                .clone()
                .ok_or_else(|| anyhow!("missing pool key for Uniswap V4 step"))?,
        })),
        VenueKind::UniswapV2 => Ok(Arc::new(UniswapV2LikeAdapter {
            name: step.name.clone(),
            router: step.router,
            path: if step.path.is_empty() {
                vec![step.token_in, step.token_out]
            } else {
                step.path.clone()
            },
        })),
        VenueKind::Virtuals | VenueKind::Unknown => Err(anyhow!(
            "no production adapter registered for {:?}",
            step.venue
        )),
    }
}

pub fn default_quote_steps(token: Address, pool_key: Option<PoolKey>) -> Vec<RouteStep> {
    let (currency0, currency1) = if token < *WETH_BASE {
        (token, *WETH_BASE)
    } else {
        (*WETH_BASE, token)
    };

    let mut steps = vec![
        RouteStep {
            venue: VenueKind::UniswapV3,
            name: "UniswapV3 1%".to_string(),
            router: *UNIV3_ROUTER,
            quoter: Some(*UNIV3_QUOTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: 100,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: None,
            estimated_gas: 160_000,
        },
        RouteStep {
            venue: VenueKind::UniswapV3,
            name: "PancakeSwapV3".to_string(),
            router: *PANCAKESWAP_V3_ROUTER,
            quoter: Some(*PANCAKESWAP_V3_QUOTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: 25,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: None,
            estimated_gas: 170_000,
        },
        RouteStep {
            venue: VenueKind::AerodromeV2,
            name: "Aerodrome V2".to_string(),
            router: *AERODROME_ROUTER,
            quoter: Some(*AERODROME_ROUTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: 30,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: None,
            estimated_gas: 180_000,
        },
    ];

    if let Some(pool_key) = pool_key {
        steps.push(RouteStep {
            venue: VenueKind::UniswapV4,
            name: "Extracted V4".to_string(),
            router: *UNIVERSAL_ROUTER,
            quoter: Some(*UNIV4_QUOTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: pool_key.fee,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: Some(pool_key),
            estimated_gas: 220_000,
        });
    } else {
        steps.push(RouteStep {
            venue: VenueKind::UniswapV4,
            name: "Clanker Static".to_string(),
            router: *UNIVERSAL_ROUTER,
            quoter: Some(*UNIV4_QUOTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: 100,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: Some(PoolKey {
                currency0,
                currency1,
                fee: 10_000,
                tick_spacing: 200,
                hooks: *CLANKER_HOOK_STATIC,
            }),
            estimated_gas: 220_000,
        });
        steps.push(RouteStep {
            venue: VenueKind::UniswapV4,
            name: "Clanker Dynamic".to_string(),
            router: *UNIVERSAL_ROUTER,
            quoter: Some(*UNIV4_QUOTER),
            token_in: *WETH_BASE,
            token_out: token,
            fee_bps: 100,
            path: vec![*WETH_BASE, token],
            stable: false,
            pool_key: Some(PoolKey {
                currency0,
                currency1,
                fee: 0x800000,
                tick_spacing: 200,
                hooks: *CLANKER_HOOK_DYNAMIC,
            }),
            estimated_gas: 220_000,
        });
    }

    steps
}

pub struct UniswapV3Adapter {
    pub name: String,
    pub router: Address,
    pub quoter: Address,
    pub fee: u32,
}

impl ExecutionAdapter for UniswapV3Adapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn venue(&self) -> VenueKind {
        VenueKind::UniswapV3
    }

    fn encode_quote(&self, amount_in: U256, token_in: Address, token_out: Address) -> Result<ContractCall> {
        let params = (token_in, token_out, amount_in, self.fee, U256::zero());
        let calldata = BaseContract::from(v3_quoter_abi()).encode("quoteExactInputSingle", (params,))?;
        Ok(ContractCall {
            target: self.quoter,
            calldata: calldata.0.into(),
            value: U256::zero(),
        })
    }

    fn decode_quote(&self, output: Bytes) -> Result<U256> {
        let (amount_out, _, _, _): (U256, U256, u32, U256) =
            BaseContract::from(v3_quoter_abi()).decode_output("quoteExactInputSingle", output)?;
        Ok(amount_out)
    }

    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        token_out: Address,
        recipient: Address,
        amount_out_min: U256,
        _deadline: U256,
    ) -> Result<ContractCall> {
        let params = AbiToken::Tuple(vec![
            AbiToken::Address(token_in),
            AbiToken::Address(token_out),
            AbiToken::Uint(U256::from(self.fee)),
            AbiToken::Address(recipient),
            AbiToken::Uint(amount_in),
            AbiToken::Uint(amount_out_min),
            AbiToken::Uint(U256::zero()),
        ]);
        let abi = v3_router_abi();
        let function = abi.function("exactInputSingle")?;
        Ok(ContractCall {
            target: self.router,
            calldata: function.encode_input(&[params])?.into(),
            value: if token_in == *WETH_BASE {
                amount_in
            } else {
                U256::zero()
            },
        })
    }
}

pub struct UniswapV2LikeAdapter {
    pub name: String,
    pub router: Address,
    pub path: Vec<Address>,
}

impl ExecutionAdapter for UniswapV2LikeAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn venue(&self) -> VenueKind {
        VenueKind::UniswapV2
    }

    fn encode_quote(&self, amount_in: U256, _token_in: Address, _token_out: Address) -> Result<ContractCall> {
        let calldata = BaseContract::from(v2_router_abi()).encode("getAmountsOut", (amount_in, self.path.clone()))?;
        Ok(ContractCall {
            target: self.router,
            calldata: calldata.0.into(),
            value: U256::zero(),
        })
    }

    fn decode_quote(&self, output: Bytes) -> Result<U256> {
        let amounts: Vec<U256> = BaseContract::from(v2_router_abi()).decode_output("getAmountsOut", output)?;
        Ok(*amounts.last().unwrap_or(&U256::zero()))
    }

    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        _token_out: Address,
        recipient: Address,
        amount_out_min: U256,
        deadline: U256,
    ) -> Result<ContractCall> {
        let (function_name, tokens, value) = if token_in == *WETH_BASE {
            (
                "swapExactETHForTokensSupportingFeeOnTransferTokens",
                vec![amount_out_min.into_token(), self.path.clone().into_token(), recipient.into_token(), deadline.into_token()],
                amount_in,
            )
        } else {
            (
                "swapExactTokensForETHSupportingFeeOnTransferTokens",
                vec![amount_in.into_token(), amount_out_min.into_token(), self.path.clone().into_token(), recipient.into_token(), deadline.into_token()],
                U256::zero(),
            )
        };
        let abi = v2_router_abi();
        let function = abi.function(function_name)?;
        Ok(ContractCall {
            target: self.router,
            calldata: function.encode_input(&tokens)?.into(),
            value,
        })
    }
}

pub struct AerodromeV2Adapter {
    pub name: String,
    pub router: Address,
    pub factory: Address,
    pub path: Vec<Address>,
    pub stable: bool,
}

impl ExecutionAdapter for AerodromeV2Adapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn venue(&self) -> VenueKind {
        VenueKind::AerodromeV2
    }

    fn encode_quote(&self, amount_in: U256, token_in: Address, _token_out: Address) -> Result<ContractCall> {
        let routes = aerodrome_routes(&self.path, self.factory, self.stable, token_in);
        let calldata = BaseContract::from(aerodrome_abi()).encode("getAmountsOut", (amount_in, routes))?;
        Ok(ContractCall {
            target: self.router,
            calldata: calldata.0.into(),
            value: U256::zero(),
        })
    }

    fn decode_quote(&self, output: Bytes) -> Result<U256> {
        let amounts: Vec<U256> =
            BaseContract::from(aerodrome_abi()).decode_output("getAmountsOut", output)?;
        Ok(*amounts.last().unwrap_or(&U256::zero()))
    }

    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        _token_out: Address,
        recipient: Address,
        amount_out_min: U256,
        deadline: U256,
    ) -> Result<ContractCall> {
        if token_in == *WETH_BASE {
            let routes = aerodrome_routes(&self.path, self.factory, self.stable, token_in);
            let calldata = BaseContract::from(aerodrome_abi()).encode(
                "swapExactETHForTokensSupportingFeeOnTransferTokens",
                (amount_out_min, routes, recipient, deadline),
            )?;
            Ok(ContractCall {
                target: self.router,
                calldata: calldata.0.into(),
                value: amount_in,
            })
        } else {
            let routes = aerodrome_routes(&self.path, self.factory, self.stable, token_in);
            let calldata = BaseContract::from(aerodrome_abi()).encode(
                "swapExactTokensForETHSupportingFeeOnTransferTokens",
                (amount_in, amount_out_min, routes, recipient, deadline),
            )?;
            Ok(ContractCall {
                target: self.router,
                calldata: calldata.0.into(),
                value: U256::zero(),
            })
        }
    }
}

pub struct UniswapV4Adapter {
    pub name: String,
    pub pool_key: PoolKey,
}

impl ExecutionAdapter for UniswapV4Adapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn venue(&self) -> VenueKind {
        VenueKind::UniswapV4
    }

    fn encode_quote(&self, amount_in: U256, token_in: Address, token_out: Address) -> Result<ContractCall> {
        let zero_for_one = token_in < token_out;
        let pool_key = encode_pool_key(&self.pool_key);
        let params = AbiToken::Tuple(vec![
            pool_key,
            AbiToken::Bool(zero_for_one),
            AbiToken::Uint(amount_in),
            AbiToken::Bytes(vec![]),
        ]);
        let abi = v4_quoter_abi();
        let function = abi.function("quoteExactInputSingle")?;
        Ok(ContractCall {
            target: *UNIV4_QUOTER,
            calldata: function.encode_input(&[params])?.into(),
            value: U256::zero(),
        })
    }

    fn decode_quote(&self, output: Bytes) -> Result<U256> {
        let (amount_out, _): (U256, u128) =
            BaseContract::from(v4_quoter_abi()).decode_output("quoteExactInputSingle", output)?;
        Ok(amount_out)
    }

    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        token_out: Address,
        _recipient: Address,
        amount_out_min: U256,
        deadline: U256,
    ) -> Result<ContractCall> {
        let zero_for_one = token_in < token_out;
        let swap_params = ethers::abi::encode(&[
            encode_pool_key(&self.pool_key),
            AbiToken::Bool(zero_for_one),
            AbiToken::Uint(amount_in),
            AbiToken::Uint(amount_out_min),
            AbiToken::Bytes(Vec::<u8>::new()),
        ]);
        let v4_input = ethers::abi::encode(&[
            AbiToken::Bytes(vec![0x06u8]),
            AbiToken::Array(vec![AbiToken::Bytes(swap_params)]),
        ]);
        let calldata = BaseContract::from(universal_router_abi()).encode(
            "execute",
            (
                Bytes::from(vec![0x10u8]),
                vec![AbiToken::Bytes(v4_input)],
                deadline,
            ),
        )?;
        Ok(ContractCall {
            target: *UNIVERSAL_ROUTER,
            calldata: calldata.0.into(),
            value: if token_in == *WETH_BASE {
                amount_in
            } else {
                U256::zero()
            },
        })
    }
}

fn encode_pool_key(pool_key: &PoolKey) -> AbiToken {
    AbiToken::Tuple(vec![
        AbiToken::Address(pool_key.currency0),
        AbiToken::Address(pool_key.currency1),
        AbiToken::Uint(U256::from(pool_key.fee)),
        AbiToken::Int(U256::from(pool_key.tick_spacing as u32)),
        AbiToken::Address(pool_key.hooks),
    ])
}

fn aerodrome_routes(
    path: &[Address],
    factory: Address,
    stable: bool,
    token_in: Address,
) -> Vec<(Address, Address, bool, Address)> {
    let mut effective_path = path.to_vec();
    if !effective_path.is_empty() && effective_path[0] != token_in {
        effective_path.reverse();
    }

    effective_path
        .windows(2)
        .map(|window| (window[0], window[1], stable, factory))
        .collect()
}

fn v3_quoter_abi() -> Abi {
    let mut abi = Abi::default();
    let params_type = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(24),
        ParamType::Uint(160),
    ]);
    let function = Function {
        name: "quoteExactInputSingle".to_string(),
        inputs: vec![Param {
            name: "params".to_string(),
            kind: params_type,
            internal_type: None,
        }],
        outputs: vec![
            Param {
                name: "amountOut".to_string(),
                kind: ParamType::Uint(256),
                internal_type: None,
            },
            Param {
                name: "sqrtPriceX96After".to_string(),
                kind: ParamType::Uint(160),
                internal_type: None,
            },
            Param {
                name: "initializedTicksCrossed".to_string(),
                kind: ParamType::Uint(32),
                internal_type: None,
            },
            Param {
                name: "gasEstimate".to_string(),
                kind: ParamType::Uint(256),
                internal_type: None,
            },
        ],
        constant: None,
        state_mutability: StateMutability::NonPayable,
    };
    abi.functions
        .insert("quoteExactInputSingle".to_string(), vec![function]);
    abi
}

fn v3_router_abi() -> Abi {
    let mut abi = Abi::default();
    let params_type = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ]);
    let function = Function {
        name: "exactInputSingle".to_string(),
        inputs: vec![Param {
            name: "params".to_string(),
            kind: params_type,
            internal_type: None,
        }],
        outputs: vec![Param {
            name: "amountOut".to_string(),
            kind: ParamType::Uint(256),
            internal_type: None,
        }],
        constant: None,
        state_mutability: StateMutability::Payable,
    };
    abi.functions
        .insert("exactInputSingle".to_string(), vec![function]);
    abi
}

fn v2_router_abi() -> Abi {
    let mut abi = Abi::default();
    abi.functions.insert(
        "getAmountsOut".to_string(),
        vec![Function {
            name: "getAmountsOut".to_string(),
            inputs: vec![
                Param {
                    name: "amountIn".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "path".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Address)),
                    internal_type: None,
                },
            ],
            outputs: vec![Param {
                name: "amounts".to_string(),
                kind: ParamType::Array(Box::new(ParamType::Uint(256))),
                internal_type: None,
            }],
            constant: Some(true),
            state_mutability: StateMutability::View,
        }],
    );
    abi.functions.insert(
        "swapExactETHForTokensSupportingFeeOnTransferTokens".to_string(),
        vec![Function {
            name: "swapExactETHForTokensSupportingFeeOnTransferTokens".to_string(),
            inputs: vec![
                Param {
                    name: "amountOutMin".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "path".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Address)),
                    internal_type: None,
                },
                Param {
                    name: "to".to_string(),
                    kind: ParamType::Address,
                    internal_type: None,
                },
                Param {
                    name: "deadline".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::Payable,
        }],
    );
    abi.functions.insert(
        "swapExactTokensForETHSupportingFeeOnTransferTokens".to_string(),
        vec![Function {
            name: "swapExactTokensForETHSupportingFeeOnTransferTokens".to_string(),
            inputs: vec![
                Param {
                    name: "amountIn".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "amountOutMin".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "path".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Address)),
                    internal_type: None,
                },
                Param {
                    name: "to".to_string(),
                    kind: ParamType::Address,
                    internal_type: None,
                },
                Param {
                    name: "deadline".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::NonPayable,
        }],
    );
    abi
}

fn aerodrome_abi() -> Abi {
    let mut abi = Abi::default();
    let route_type = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Bool,
        ParamType::Address,
    ]);
    abi.functions.insert(
        "getAmountsOut".to_string(),
        vec![Function {
            name: "getAmountsOut".to_string(),
            inputs: vec![
                Param {
                    name: "amountIn".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "routes".to_string(),
                    kind: ParamType::Array(Box::new(route_type.clone())),
                    internal_type: None,
                },
            ],
            outputs: vec![Param {
                name: "amounts".to_string(),
                kind: ParamType::Array(Box::new(ParamType::Uint(256))),
                internal_type: None,
            }],
            constant: Some(true),
            state_mutability: StateMutability::View,
        }],
    );
    abi.functions.insert(
        "swapExactETHForTokensSupportingFeeOnTransferTokens".to_string(),
        vec![Function {
            name: "swapExactETHForTokensSupportingFeeOnTransferTokens".to_string(),
            inputs: vec![
                Param {
                    name: "amountOutMin".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "routes".to_string(),
                    kind: ParamType::Array(Box::new(route_type.clone())),
                    internal_type: None,
                },
                Param {
                    name: "to".to_string(),
                    kind: ParamType::Address,
                    internal_type: None,
                },
                Param {
                    name: "deadline".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::Payable,
        }],
    );
    abi.functions.insert(
        "swapExactTokensForETHSupportingFeeOnTransferTokens".to_string(),
        vec![Function {
            name: "swapExactTokensForETHSupportingFeeOnTransferTokens".to_string(),
            inputs: vec![
                Param {
                    name: "amountIn".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "amountOutMin".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "routes".to_string(),
                    kind: ParamType::Array(Box::new(route_type)),
                    internal_type: None,
                },
                Param {
                    name: "to".to_string(),
                    kind: ParamType::Address,
                    internal_type: None,
                },
                Param {
                    name: "deadline".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::NonPayable,
        }],
    );
    abi
}

fn v4_quoter_abi() -> Abi {
    let mut abi = Abi::default();
    let pool_key_type = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Int(24),
        ParamType::Address,
    ]);
    let params_type = ParamType::Tuple(vec![
        pool_key_type,
        ParamType::Bool,
        ParamType::Uint(128),
        ParamType::Bytes,
    ]);
    abi.functions.insert(
        "quoteExactInputSingle".to_string(),
        vec![Function {
            name: "quoteExactInputSingle".to_string(),
            inputs: vec![Param {
                name: "params".to_string(),
                kind: params_type,
                internal_type: None,
            }],
            outputs: vec![
                Param {
                    name: "amountOut".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
                Param {
                    name: "gasEstimate".to_string(),
                    kind: ParamType::Uint(128),
                    internal_type: None,
                },
            ],
            constant: None,
            state_mutability: StateMutability::NonPayable,
        }],
    );
    abi
}

fn universal_router_abi() -> Abi {
    let mut abi = Abi::default();
    abi.functions.insert(
        "execute".to_string(),
        vec![Function {
            name: "execute".to_string(),
            inputs: vec![
                Param {
                    name: "commands".to_string(),
                    kind: ParamType::Bytes,
                    internal_type: None,
                },
                Param {
                    name: "inputs".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Bytes)),
                    internal_type: None,
                },
                Param {
                    name: "deadline".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::Payable,
        }],
    );
    abi
}
