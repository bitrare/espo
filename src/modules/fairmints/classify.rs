use crate::alkanes::trace::{EspoSandshrewLikeTrace, EspoSandshrewLikeTraceEvent};
use borsh::{BorshDeserialize, BorshSerialize};

/// Transaction type classification for alkane transactions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum TxType {
    DieselMint = 0,
    Mint = 1,
    Transfer = 2,
    Marketplace = 3,
    Swap = 4,
    Deploy = 5,
    OtherContractCall = 6,
    #[default]
    Unknown = 255,
}

impl TxType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TxType::DieselMint => "diesel_mint",
            TxType::Mint => "mint",
            TxType::Transfer => "transfer",
            TxType::Marketplace => "marketplace",
            TxType::Swap => "swap",
            TxType::Deploy => "deploy",
            TxType::OtherContractCall => "other_contract_call",
            TxType::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "diesel_mint" => Some(TxType::DieselMint),
            "mint" => Some(TxType::Mint),
            "transfer" => Some(TxType::Transfer),
            "marketplace" => Some(TxType::Marketplace),
            "swap" => Some(TxType::Swap),
            "deploy" => Some(TxType::Deploy),
            "other_contract_call" => Some(TxType::OtherContractCall),
            "unknown" => Some(TxType::Unknown),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KnownMarketplace {
    pub id: &'static str,
    pub name: &'static str,
    pub fee_addresses: &'static [&'static str],
}

pub static KNOWN_MARKETPLACES: &[KnownMarketplace] = &[
    KnownMarketplace {
        id: "idclub",
        name: "IDclub",
        fee_addresses: &[
            "bc1qtr2m8tr797v2z6tptuqlveh0egper6m6m0vtls",
            "bc1qvdyqz4uyjf40vq9h5jr5uyv9vrs8gcxne6u7pf",
            "bc1qj33vexl9gujueg40efh7s27df32thnxnw9pdc2",
        ],
    },
    KnownMarketplace {
        id: "fairmints",
        name: "Fairmints",
        fee_addresses: &[
            "bc1qnnja0erwudzd04m5j8n3jyk7zh7c4jl395vve2",
            "bc1q05adsgqpnyxwcvrdurwmtc6swfne9llhy0t8nk",
        ],
    },
];

pub fn lookup_marketplace_by_fee_address(address: &str) -> Option<(&'static str, &'static str)> {
    for mp in KNOWN_MARKETPLACES {
        if mp.fee_addresses.contains(&address) {
            return Some((mp.id, mp.name));
        }
    }
    None
}

#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct TxMarketplaceInfo {
    pub marketplace_id: String,
    pub marketplace_name: String,
    pub fee_address: String,
    pub fee_sats: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct TxClassification {
    pub tx_type: TxType,
    pub marketplace_info: Option<TxMarketplaceInfo>,
}

#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct StoredTxClass {
    pub tx_type: TxType,
    pub marketplace_info: Option<TxMarketplaceInfo>,
}

fn parse_u128_from_str(s: &str) -> Option<u128> {
    if let Some(hex) = s.strip_prefix("0x") {
        u128::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u128>().ok()
    }
}

fn parse_u32_or_hex(s: &str) -> Option<u32> {
    if let Some(hex) = s.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

fn parse_u64_or_hex(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// A diesel mint is: contract 2:0, opcode 77, single invoke, remaining inputs 0.
pub fn is_diesel_mint_trace_sandshrew(trace: &EspoSandshrewLikeTrace) -> bool {
    let mut invoke_count = 0usize;
    let mut contract_block: Option<u32> = None;
    let mut contract_tx: Option<u64> = None;
    let mut inputs: Option<&[String]> = None;

    for ev in &trace.events {
        if let EspoSandshrewLikeTraceEvent::Invoke(data) = ev {
            invoke_count += 1;
            if invoke_count > 1 {
                return false;
            }
            contract_block = parse_u32_or_hex(&data.context.myself.block);
            contract_tx = parse_u64_or_hex(&data.context.myself.tx);
            inputs = Some(&data.context.inputs);
        }
    }

    if invoke_count != 1 {
        return false;
    }
    if contract_block != Some(2) || contract_tx != Some(0) {
        return false;
    }
    let Some(inputs) = inputs else {
        return false;
    };
    if inputs.is_empty() {
        return false;
    }
    let mut iter = inputs.iter();
    let opcode = iter.next().and_then(|s| parse_u128_from_str(s)).unwrap_or_default();
    if opcode != 77 {
        return false;
    }
    iter.all(|s| parse_u128_from_str(s).map_or(false, |v| v == 0))
}

pub fn is_diesel_mint_tx_sandshrew(traces: &[EspoSandshrewLikeTrace]) -> bool {
    traces.len() == 1 && is_diesel_mint_trace_sandshrew(&traces[0])
}

pub fn classify_transaction(
    traces: &[EspoSandshrewLikeTrace],
    output_addresses: &[(String, u64)],
) -> TxClassification {
    if is_diesel_mint_tx_sandshrew(traces) {
        return TxClassification { tx_type: TxType::DieselMint, marketplace_info: None };
    }

    let mut has_create = false;
    let mut has_invoke = false;
    let mut invoke_opcode: Option<u128> = None;

    for trace in traces {
        for ev in &trace.events {
            match ev {
                EspoSandshrewLikeTraceEvent::Create(_) => {
                    has_create = true;
                }
                EspoSandshrewLikeTraceEvent::Invoke(data) => {
                    has_invoke = true;
                    if let Some(first_input) = data.context.inputs.first() {
                        invoke_opcode = parse_u128_from_str(first_input);
                    }
                }
                _ => {}
            }
        }
    }

    if has_create {
        return TxClassification { tx_type: TxType::Deploy, marketplace_info: None };
    }

    if has_invoke && invoke_opcode == Some(77) {
        return TxClassification { tx_type: TxType::Mint, marketplace_info: None };
    }

    for (address, sats) in output_addresses {
        if let Some((mp_id, mp_name)) = lookup_marketplace_by_fee_address(address) {
            return TxClassification {
                tx_type: TxType::Marketplace,
                marketplace_info: Some(TxMarketplaceInfo {
                    marketplace_id: mp_id.to_string(),
                    marketplace_name: mp_name.to_string(),
                    fee_address: address.clone(),
                    fee_sats: Some(*sats),
                }),
            };
        }
    }

    if has_invoke {
        return TxClassification { tx_type: TxType::OtherContractCall, marketplace_info: None };
    }

    if !traces.is_empty() {
        return TxClassification { tx_type: TxType::Transfer, marketplace_info: None };
    }

    TxClassification { tx_type: TxType::Unknown, marketplace_info: None }
}
