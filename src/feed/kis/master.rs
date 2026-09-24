//! KIS instrument master files: KRX (fixed width) and US (tab separated), both cp949.

use std::io::Read;

use encoding_rs::EUC_KR;

use crate::domain::{InstrumentId, Venue};
use crate::venue::{Instrument, TickRule, whole_shares};

pub const MASTER_BASE: &str = "https://new.real.download.dws.co.kr/common/master";

/// The first member of a zip archive.
pub fn unzip_first(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut out = Vec::new();
    archive.by_index(0)?.read_to_end(&mut out)?;
    Ok(out)
}

/// KOSPI/KOSDAQ master lines: short code in bytes 0..9, name in bytes 21..61. Funds and other
/// non-6-character codes are dropped.
pub fn parse_krx_master(bytes: &[u8]) -> Vec<Instrument> {
    bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.len() < 61 {
                return None;
            }
            let code = std::str::from_utf8(&line[0..9]).ok()?.trim();
            if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            let (name, _, _) = EUC_KR.decode(&line[21..61]);
            Some(Instrument {
                id: InstrumentId { venue: Venue::Krx, symbol: code.to_string() },
                name: name.trim().to_string(),
                tick: TickRule::Krx,
                lot: whole_shares(),
                tradable: true,
            })
        })
        .collect()
}

/// US master rows (NAS/NYS/AMS): keeps USD stocks (type 2) and ETPs (type 3), with the
/// exchange code KIS wants in requests.
pub fn parse_us_master(bytes: &[u8]) -> Vec<(Instrument, String)> {
    let (text, _, _) = EUC_KR.decode(bytes);
    text.lines()
        .filter_map(|line| {
            let c: Vec<&str> = line.split('\t').map(str::trim).collect();
            if c.len() < 10 || !matches!(c[8], "2" | "3") || c[9] != "USD" || c[4].is_empty() {
                return None;
            }
            let name = match (c[6], c[7]) {
                (ko, en) if !ko.is_empty() && ko != en => format!("{ko} ({en})"),
                (_, en) => en.to_string(),
            };
            let inst = Instrument {
                id: InstrumentId { venue: Venue::Us, symbol: c[4].to_string() },
                name,
                tick: TickRule::Us,
                lot: whole_shares(),
                tradable: true,
            };
            Some((inst, c[2].to_string()))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn krx_master_keeps_six_char_codes_with_korean_names() {
        let mut bytes = include_bytes!("../../../tests/fixtures/kis/kospi_sample.mst").to_vec();
        bytes.extend_from_slice(include_bytes!("../../../tests/fixtures/kis/kosdaq_sample.mst"));
        let list = parse_krx_master(&bytes);
        let names: Vec<(String, String)> = list.iter().map(|i| (i.id.to_string(), i.name.clone())).collect();
        assert_eq!(
            names,
            vec![
                ("KRX:005930".to_string(), "삼성전자".to_string()),
                ("KRX:000660".to_string(), "SK하이닉스".to_string()),
                ("KRX:900110".to_string(), "딥커머스".to_string()),
            ]
        );
        assert_eq!(list[0].tick, TickRule::Krx);
    }

    #[test]
    fn us_master_keeps_usd_stocks_and_etps_with_exchange() {
        let list = parse_us_master(include_bytes!("../../../tests/fixtures/kis/nas_sample.cod"));
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].0.id.to_string(), "US:AAPL");
        assert_eq!(list[0].0.name, "애플 (APPLE INC)");
        assert_eq!(list[0].1, "NAS");
        assert_eq!(list[0].0.tick, TickRule::Us);
    }

    #[test]
    fn unzip_reads_the_first_member() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("kospi_code.mst", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"hello").unwrap();
            z.finish().unwrap();
        }
        assert_eq!(unzip_first(buf.get_ref()).unwrap(), b"hello");
        assert!(unzip_first(b"not a zip").is_err());
    }
}
