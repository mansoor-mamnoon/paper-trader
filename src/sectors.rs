//! The sectors the newsbook follows. Each stock belongs to exactly one.

use crate::market::Listing;
use serde::Serialize;

/// One sector of the newsbook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Sector {
    /// Short stable id used in the database and in model answers.
    pub id: &'static str,
    pub name: &'static str,
    /// News search phrase for the sector.
    pub query: &'static str,
    /// Tickers that belong here whatever the exchange's industry label says.
    pub tickers: &'static [&'static str],
    /// Words in a company name that place it here.
    pub name_words: &'static [&'static str],
    /// Words in the exchange's industry label that place a stock here.
    pub industry_words: &'static [&'static str],
    /// Exchange sector labels that place a stock here when nothing above matched.
    pub exchange_sectors: &'static [&'static str],
}

pub const SECTORS: [Sector; 12] = [
    Sector {
        id: "chips",
        name: "AI chips and semiconductors",
        query: "semiconductor OR \"AI chips\" OR \"data center\" stocks",
        tickers: &[
            "NVDA", "AMD", "AVGO", "TSM", "MU", "ARM", "SMCI", "ASML", "INTC", "MRVL",
        ],
        name_words: &["semiconductor"],
        industry_words: &["semiconductor"],
        exchange_sectors: &[],
    },
    Sector {
        id: "software",
        name: "Software, cloud and AI applications",
        query: "software OR cloud OR \"artificial intelligence\" company stock",
        tickers: &[
            "MSFT", "GOOGL", "GOOG", "META", "PLTR", "ORCL", "CRM", "SNOW", "NOW", "APP",
        ],
        name_words: &[],
        industry_words: &["software", "edp services", "computer", "internet"],
        exchange_sectors: &["Technology"],
    },
    Sector {
        id: "frontier",
        name: "Quantum, space and defense",
        query: "quantum computing OR space OR defense contractor stocks",
        tickers: &[
            "IONQ", "RGTI", "QBTS", "QUBT", "RKLB", "ASTS", "LUNR", "LMT", "RTX", "NOC", "GD",
            "KTOS", "AVAV", "BA", "PL", "RDW",
        ],
        name_words: &["quantum", "aerospace", "defense"],
        industry_words: &["aerospace", "military", "ordnance"],
        exchange_sectors: &[],
    },
    Sector {
        id: "clean",
        name: "Clean energy, nuclear and power",
        query: "renewable energy OR solar OR nuclear power OR utilities legislation stocks",
        tickers: &[
            "FSLR", "ENPH", "NEE", "CEG", "VST", "OKLO", "SMR", "CCJ", "BE", "PLUG", "RUN", "SEDG",
            "NXT", "GEV", "TLN", "LEU", "NNE", "FLNC", "EOSE",
        ],
        name_words: &["solar", "nuclear", "uranium", "hydrogen", "renewable"],
        industry_words: &["electric utilities", "power generation", "water supply"],
        exchange_sectors: &["Utilities"],
    },
    Sector {
        id: "oil",
        name: "Oil, gas and commodities energy",
        query: "oil OR \"natural gas\" OR OPEC energy stocks",
        tickers: &[],
        name_words: &[],
        industry_words: &["oil", "natural gas", "coal", "petroleum"],
        exchange_sectors: &["Energy"],
    },
    Sector {
        id: "biotech",
        name: "Biotech and pharma",
        query: "biotech OR FDA approval OR drug trial stocks",
        tickers: &["LLY", "NVO", "MRNA", "VKTX", "HIMS"],
        name_words: &["therapeutics", "pharma", "biosciences", "biotech"],
        industry_words: &["biotechnology", "pharmaceutical"],
        exchange_sectors: &[],
    },
    Sector {
        id: "health",
        name: "Healthcare and medical technology",
        query: "healthcare OR medical device OR health insurer stocks",
        tickers: &[],
        name_words: &[],
        industry_words: &["medical", "hospital", "health"],
        exchange_sectors: &["Health Care"],
    },
    Sector {
        id: "crypto",
        name: "Crypto-linked and fintech",
        query: "bitcoin OR crypto OR fintech stocks",
        tickers: &[
            "COIN", "MSTR", "MARA", "RIOT", "HOOD", "CLSK", "SOFI", "AFRM", "UPST", "PYPL", "XYZ",
            "CRCL", "IREN", "CIFR", "WULF", "BMNR", "GLXY",
        ],
        name_words: &["bitcoin", "crypto", "blockchain", "digital asset"],
        industry_words: &[],
        exchange_sectors: &[],
    },
    Sector {
        id: "finance",
        name: "Banks, insurers and real estate",
        query: "banks OR Federal Reserve rates OR insurers OR REIT stocks",
        tickers: &[],
        name_words: &[],
        industry_words: &["bank", "insur", "real estate", "investment"],
        exchange_sectors: &["Finance", "Real Estate"],
    },
    Sector {
        id: "autos",
        name: "Electric vehicles, autos and robotics",
        query: "electric vehicle OR robotaxi OR humanoid robot OR automaker stocks",
        tickers: &[
            "TSLA", "RIVN", "LCID", "NIO", "XPEV", "LI", "GM", "F", "SERV", "RR", "SYM",
        ],
        name_words: &["robotics", "motors", "automotive"],
        industry_words: &["auto manufacturing", "auto parts"],
        exchange_sectors: &[],
    },
    Sector {
        id: "consumer",
        name: "Consumer, retail, travel and media",
        query: "retail sales OR consumer spending OR airline OR streaming stocks",
        tickers: &["AMZN", "NFLX", "DIS", "UBER", "ABNB"],
        name_words: &[],
        industry_words: &[],
        exchange_sectors: &[
            "Consumer Discretionary",
            "Consumer Staples",
            "Telecommunications",
        ],
    },
    Sector {
        id: "industry",
        name: "Industrials, infrastructure and materials",
        query: "infrastructure spending OR manufacturing OR tariffs OR metals mining stocks",
        tickers: &[],
        name_words: &[],
        industry_words: &[],
        exchange_sectors: &["Industrials", "Basic Materials", "Miscellaneous"],
    },
];

/// The sector with this id.
pub fn by_id(id: &str) -> Option<&'static Sector> {
    SECTORS.iter().find(|s| s.id == id)
}

/// The sector's display name, or the id when it is unknown.
pub fn name_of(id: &str) -> &str {
    by_id(id).map_or(id, |s| s.name)
}

/// The sector a stock belongs to. Explicit tickers win, then words in the company name,
/// then the exchange's industry label, then its sector label.
pub fn sector_of(listing: &Listing) -> &'static Sector {
    let name = listing.name.to_lowercase();
    let industry = listing.industry.to_lowercase();
    SECTORS
        .iter()
        .find(|s| s.tickers.contains(&listing.symbol.as_str()))
        .or_else(|| {
            SECTORS
                .iter()
                .find(|s| s.name_words.iter().any(|w| name.contains(w)))
        })
        .or_else(|| {
            SECTORS
                .iter()
                .find(|s| s.industry_words.iter().any(|w| industry.contains(w)))
        })
        .or_else(|| {
            SECTORS
                .iter()
                .find(|s| s.exchange_sectors.contains(&listing.sector.as_str()))
        })
        .unwrap_or(&SECTORS[SECTORS.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(symbol: &str, name: &str, sector: &str, industry: &str) -> Listing {
        Listing {
            symbol: symbol.into(),
            name: name.into(),
            sector: sector.into(),
            industry: industry.into(),
            ..Listing::default()
        }
    }

    #[test]
    fn ids_are_unique_and_resolvable() {
        let ids: std::collections::BTreeSet<&str> = SECTORS.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), SECTORS.len());
        assert_eq!(name_of("chips"), "AI chips and semiconductors");
        assert_eq!(name_of("nope"), "nope");
    }

    #[test]
    fn stocks_land_in_the_expected_sector() {
        let cases = [
            (
                "NVDA",
                "NVIDIA Corporation Common Stock",
                "Technology",
                "Semiconductors",
                "chips",
            ),
            (
                "FSLR",
                "First Solar Inc. Common Stock",
                "Technology",
                "Semiconductors",
                "clean",
            ),
            (
                "XYZQ",
                "Acme Software Inc. Common Stock",
                "Technology",
                "Computer Software: Prepackaged Software",
                "software",
            ),
            (
                "COIN",
                "Coinbase Global Inc. Class A Common Stock",
                "Finance",
                "Finance: Consumer Services",
                "crypto",
            ),
            (
                "JPM",
                "JP Morgan Chase & Co. Common Stock",
                "Finance",
                "Major Banks",
                "finance",
            ),
            (
                "XOM",
                "Exxon Mobil Corporation Common Stock",
                "Energy",
                "Integrated oil Companies",
                "oil",
            ),
            (
                "ABCD",
                "Acme Therapeutics Inc. Common Stock",
                "Health Care",
                "Biotechnology: Pharmaceutical Preparations",
                "biotech",
            ),
            (
                "UNH",
                "UnitedHealth Group Incorporated Common Stock",
                "Health Care",
                "Medical Specialities",
                "health",
            ),
            (
                "TSLA",
                "Tesla Inc. Common Stock",
                "Consumer Discretionary",
                "Auto Manufacturing",
                "autos",
            ),
            (
                "WMT",
                "Walmart Inc. Common Stock",
                "Consumer Discretionary",
                "Department/Specialty Retail Stores",
                "consumer",
            ),
            (
                "CAT",
                "Caterpillar Inc. Common Stock",
                "Industrials",
                "Construction/Ag Equipment/Trucks",
                "industry",
            ),
            (
                "RKLB",
                "Rocket Lab Corporation Common Stock",
                "Industrials",
                "Aerospace",
                "frontier",
            ),
            ("ZZZZ", "Unknown Co Common Stock", "", "", "industry"),
        ];
        for (symbol, name, sector, industry, expected) in cases {
            assert_eq!(
                sector_of(&listing(symbol, name, sector, industry)).id,
                expected,
                "{symbol}"
            );
        }
    }
}
