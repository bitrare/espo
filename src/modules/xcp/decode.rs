use bitcoin::blockdata::script::Instruction;
use bitcoin::hashes::Hash;
use bitcoin::{Transaction, Txid, opcodes};
use serde_json::{Value, json};

const CNTRPRTY: &[u8] = b"CNTRPRTY";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedXcp {
    pub message_id: u32,
    pub message_name: &'static str,
    pub payload: Vec<u8>,
}

impl DecodedXcp {
    pub fn to_json(&self) -> Value {
        json!({
            "message_name": self.message_name,
            "id": self.message_id,
            "params": { "data": hex::encode(&self.payload) },
        })
    }
}

/// RC4 key is the first input's previous txid in Bitcoin RPC / display byte order
/// (hex-decoded `txid.to_string()`), not the crate's internal `to_byte_array()`.
pub fn rc4_key_from_txid(txid: &Txid) -> Vec<u8> {
    hex::decode(txid.to_string()).unwrap_or_else(|_| txid.to_byte_array().to_vec())
}

pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut i: u8 = 0;
    j = 0;
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            let t = s[i as usize].wrapping_add(s[j as usize]);
            byte ^ s[t as usize]
        })
        .collect()
}

pub fn decode_tx(tx: &Transaction) -> Option<DecodedXcp> {
    let first = tx.input.first()?;
    if first.previous_output.is_null() {
        return None;
    }
    let display_key = rc4_key_from_txid(&first.previous_output.txid);
    let internal_key = first.previous_output.txid.to_byte_array().to_vec();
    let ascii_key = first.previous_output.txid.to_string().into_bytes();

    for push in op_return_pushes(tx) {
        if let Some(decoded) = try_plain(&push) {
            return Some(decoded);
        }
        for key in [&display_key, &internal_key, &ascii_key] {
            if let Some(decoded) = try_plain(&rc4(key, &push)) {
                return Some(decoded);
            }
        }
    }
    None
}

fn try_plain(plain: &[u8]) -> Option<DecodedXcp> {
    let rest = plain.strip_prefix(CNTRPRTY)?;
    parse_message(rest)
}

fn parse_message(msg: &[u8]) -> Option<DecodedXcp> {
    if msg.is_empty() {
        return None;
    }
    let (message_id, payload) = if msg[0] > 0 {
        (u32::from(msg[0]), msg[1..].to_vec())
    } else if msg.len() >= 5 {
        (u32::from_be_bytes(msg[1..5].try_into().ok()?), msg[5..].to_vec())
    } else {
        return None;
    };
    Some(DecodedXcp { message_id, message_name: message_name(message_id), payload })
}

fn op_return_pushes(tx: &Transaction) -> Vec<Vec<u8>> {
    tx.output
        .iter()
        .filter_map(|output| {
            let mut instructions = output.script_pubkey.instructions();
            match instructions.next() {
                Some(Ok(Instruction::Op(opcodes::all::OP_RETURN))) => {}
                _ => return None,
            }
            match instructions.next() {
                Some(Ok(Instruction::Op(opcodes::all::OP_PUSHNUM_13))) => return None,
                Some(Ok(Instruction::PushBytes(pb))) => {
                    let mut data = pb.as_bytes().to_vec();
                    for instr in instructions {
                        match instr {
                            Ok(Instruction::PushBytes(more)) => {
                                data.extend_from_slice(more.as_bytes())
                            }
                            _ => {}
                        }
                    }
                    if data.is_empty() { None } else { Some(data) }
                }
                _ => None,
            }
        })
        .collect()
}

pub fn message_name(id: u32) -> &'static str {
    match id {
        0 => "send",
        2 => "enhanced_send",
        3 => "mpma_send",
        4 => "sweep",
        10 => "order",
        11 => "btcpay",
        12 => "dispenser",
        13 => "dispense",
        20 | 21 => "issuance",
        22 => "fairminter",
        23 => "fairmint",
        24 => "utxo",
        25 => "attach",
        26 => "detach",
        30 => "broadcast",
        40 => "bet",
        50 => "dividend",
        60 => "burn",
        70 => "cancel",
        80 => "rps",
        90 => "rpsresolve",
        100 => "publish",
        101 => "execute",
        110 => "destroy",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_roundtrip() {
        let key = b"Key";
        let plain = b"Plaintext";
        let cipher = rc4(key, plain);
        assert_eq!(rc4(key, &cipher), plain);
    }

    #[test]
    fn parse_one_byte_dispense() {
        let mut msg = vec![13u8, 0x00];
        let decoded = parse_message(&msg).expect("dispense");
        assert_eq!(decoded.message_id, 13);
        assert_eq!(decoded.message_name, "dispense");
        assert_eq!(decoded.payload, vec![0x00]);
        msg.insert(0, 0);
        assert!(parse_message(&msg).is_none());
    }

    #[test]
    fn parse_four_byte_id() {
        let mut msg = vec![0, 0, 0, 0, 13, 0xab];
        let decoded = parse_message(&msg).expect("extended id");
        assert_eq!(decoded.message_id, 13);
        assert_eq!(decoded.payload, vec![0xab]);
        msg[4] = 20;
        let decoded = parse_message(&msg).expect("issuance");
        assert_eq!(decoded.message_name, "issuance");
    }
}
