//! The pool's settings file, `pool.toml`.
//!
//! Everything is checked before anything starts. A problem is reported as a
//! sentence that names the section and the key, and every problem found is
//! reported at once rather than one per attempt.

use pool_accountant::{DEFAULT_FEE_BP, DEFAULT_WINDOW_DAA};
use pool_chain::{BridgeSettings, Chain, StratumPort};
use pool_domain::{Amount, MinerAddress};
use pool_payout::{DEFAULT_MIN_PAYOUT_COINS, Schedule};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_PAYOUT_CRON: &str = "0 5 * * *";
const DEFAULT_API_BIND: &str = "127.0.0.1:8788";
const DEFAULT_SHARES_PER_MIN: u32 = 30;
const DEFAULT_BLOCK_WAIT_MS: u64 = 1000;
/// Shortest and longest share window accepted, in DAA steps: at least as long as
/// a reward takes to mature, and not so long that the share table grows without end.
const MIN_WINDOW_DAA: u64 = 1_000;
const MAX_WINDOW_DAA: u64 = 10_000_000;

// ---- what the file says ---------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    chain: RawChain,
    network: RawNetwork,
    #[serde(default)]
    node: RawNode,
    pool: RawPool,
    #[serde(default)]
    api: RawApi,
    stratum: RawStratum,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChain {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNetwork {
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawNode {
    grpc: Option<String>,
    wrpc: Option<String>,
}

#[derive(Deserialize)]
struct RawPool {
    address: String,
    fee_bp: Option<u32>,
    window_daa: Option<u64>,
    payout_cron: Option<String>,
    data_dir: String,
    /// The minimum payout is named after the chain's ticker, so it lands here.
    #[serde(flatten)]
    other: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawApi {
    bind: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStratum {
    shares_per_min: Option<u32>,
    var_diff: Option<bool>,
    pow2_clamp: Option<bool>,
    extranonce_size: Option<u8>,
    block_wait_time_ms: Option<u64>,
    print_stats: Option<bool>,
    #[serde(default)]
    instances: Vec<RawInstance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstance {
    port: RawPort,
    min_share_diff: f64,
}

/// A port may be written as a number or as text.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawPort {
    Number(i64),
    Text(String),
}

// ---- what the pool uses ---------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StratumConfig {
    pub shares_per_min: u32,
    pub var_diff: bool,
    pub pow2_clamp: bool,
    pub extranonce_size: u8,
    pub block_wait_time: Duration,
    pub print_stats: bool,
    pub ports: Vec<StratumPort>,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub network: String,
    pub node_grpc: String,
    pub node_wrpc: String,
    pub pool_address: MinerAddress,
    pub fee_bp: u16,
    pub window_daa: u64,
    pub min_payout: Amount,
    pub payout_schedule: Schedule,
    pub payout_cron: String,
    pub data_dir: PathBuf,
    pub api_bind: SocketAddr,
    pub stratum: StratumConfig,
    /// Things worth a warning that do not stop the pool.
    pub warnings: Vec<String>,
}

/// Everything wrong with a settings file, one sentence each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub Vec<String>);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("\n"))
    }
}

impl std::error::Error for ConfigError {}

fn port_number(listen: &str) -> Option<u16> {
    listen.rsplit_once(':').map(|(_, port)| port).unwrap_or(listen).parse().ok().filter(|p| *p != 0)
}

/// Turn what was written into the form the bridge listens on: `:5570` for every
/// interface, or `host:5570` as given.
fn listen_address(raw: &RawPort) -> Result<String, String> {
    match raw {
        RawPort::Number(n) => match u16::try_from(*n) {
            Ok(p) if p != 0 => Ok(format!(":{p}")),
            _ => Err(format!("{n} is not a port number (use 1 to 65535)")),
        },
        RawPort::Text(text) => {
            let text = text.trim();
            let listen =
                if text.chars().all(|c| c.is_ascii_digit()) && !text.is_empty() { format!(":{text}") } else { text.to_owned() };
            match port_number(&listen) {
                Some(_) if listen.starts_with(':') || listen.contains(':') => Ok(listen),
                _ => Err(format!("{text:?} is not a port (write it like :5570 or 127.0.0.1:5570)")),
            }
        }
    }
}

/// `grpc://host:port` or `host:port`, as `host:port`.
fn host_and_port(text: &str) -> Option<String> {
    let bare = text.trim().strip_prefix("grpc://").unwrap_or(text.trim());
    let (host, port) = bare.rsplit_once(':')?;
    (!host.is_empty() && port.parse::<u16>().is_ok_and(|p| p != 0)).then(|| bare.to_owned())
}

fn toml_coins_to_units(value: &toml::Value) -> Option<u64> {
    match value {
        toml::Value::Integer(n) => crate::common::coins_to_units(*n as f64),
        toml::Value::Float(f) => crate::common::coins_to_units(*f),
        _ => None,
    }
}

impl PoolConfig {
    pub fn load<C: Chain>(path: &Path) -> Result<PoolConfig, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError(vec![format!("The settings file {} could not be read: {e}", path.display())]))?;
        Self::parse::<C>(&text).map_err(|ConfigError(mut problems)| {
            problems.insert(0, format!("The settings file {} has problems:", path.display()));
            ConfigError(problems)
        })
    }

    pub fn parse<C: Chain>(text: &str) -> Result<PoolConfig, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError(vec![format!("The settings could not be read: {e}")]))?;
        let mut problems: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut problem = |text: String| problems.push(text);

        if raw.chain.name != pool_chain::BACKEND {
            problem(format!(
                "[chain] name is \"{}\", but this is the {} build of the pool. Use a build for that chain, or set name = \"{}\".",
                raw.chain.name,
                pool_chain::BACKEND,
                pool_chain::BACKEND
            ));
        }

        let network = raw.network.id.clone();
        let known = C::address_prefix(&network).is_some();
        if !known {
            problem(format!("[network] id is \"{network}\", which is not a network this pool knows. Use mainnet or testnet-10."));
        }

        let node_grpc = match &raw.node.grpc {
            Some(text) => host_and_port(text).unwrap_or_else(|| {
                problem(format!("[node] grpc is \"{text}\", which is not an address. Write it like 127.0.0.1:26210."));
                String::new()
            }),
            None => C::default_grpc_port(&network).map(|p| format!("127.0.0.1:{p}")).unwrap_or_default(),
        };
        let node_wrpc = match &raw.node.wrpc {
            Some(text) if text.starts_with("ws://") || text.starts_with("wss://") => text.clone(),
            Some(text) => {
                problem(format!("[node] wrpc is \"{text}\", which is not a websocket address. Write it like ws://127.0.0.1:27210."));
                String::new()
            }
            None => C::default_wrpc_port(&network).map(|p| format!("ws://127.0.0.1:{p}")).unwrap_or_default(),
        };

        let pool_address = if raw.pool.address.trim().is_empty() {
            problem("[pool] address is empty. Run `marigold-pool init`, then put the address it prints here.".to_owned());
            None
        } else if known {
            match MinerAddress::parse::<C>(&network, &raw.pool.address) {
                Ok(address) => Some(address),
                Err(why) => {
                    problem(format!("[pool] address is not usable: {why}"));
                    None
                }
            }
        } else {
            None
        };

        let fee_bp = raw.pool.fee_bp.unwrap_or(u32::from(DEFAULT_FEE_BP));
        let fee_bp = match u16::try_from(fee_bp).ok().filter(|f| *f <= pool_domain::MAX_FEE_BP) {
            Some(fee) => fee,
            None => {
                problem(format!(
                    "[pool] fee_bp is {fee_bp}, but it is in hundredths of a percent and cannot be more than 10000 (which is 100%)."
                ));
                DEFAULT_FEE_BP
            }
        };

        let window_daa = raw.pool.window_daa.unwrap_or(DEFAULT_WINDOW_DAA);
        if !(MIN_WINDOW_DAA..=MAX_WINDOW_DAA).contains(&window_daa) {
            problem(format!(
                "[pool] window_daa is {window_daa}, but it must be between {MIN_WINDOW_DAA} and {MAX_WINDOW_DAA} (ten steps are one second)."
            ));
        }

        // The minimum payout is named after the chain's ticker, like min_payout_magld.
        let min_key = format!("min_payout_{}", C::ticker().to_lowercase());
        let mut min_payout = Amount::from_coins(DEFAULT_MIN_PAYOUT_COINS).unwrap_or(Amount::ZERO);
        let mut stray: Vec<&String> = raw.pool.other.keys().filter(|k| **k != min_key).collect();
        stray.sort();
        for key in stray {
            problem(format!("[pool] {key} is not a setting the pool knows. (The minimum payout is called {min_key}.)"));
        }
        if let Some(value) = raw.pool.other.get(&min_key) {
            match toml_coins_to_units(value) {
                Some(units) => min_payout = Amount::new(units),
                None => problem(format!(
                    "[pool] {min_key} must be a number of coins above zero, with at most eight decimals, like 10 or 0.5."
                )),
            }
        }

        let payout_cron = raw.pool.payout_cron.clone().unwrap_or_else(|| DEFAULT_PAYOUT_CRON.to_owned());
        let payout_schedule = match Schedule::parse(&payout_cron) {
            Ok(schedule) => Some(schedule),
            Err(why) => {
                problem(format!("[pool] payout_cron: {why}"));
                None
            }
        };

        if raw.pool.data_dir.trim().is_empty() {
            problem(
                "[pool] data_dir is empty. Say where the pool keeps its database and wallet, like /var/lib/marigold/pool.".to_owned(),
            );
        }

        let api_text = raw.api.bind.clone().unwrap_or_else(|| DEFAULT_API_BIND.to_owned());
        let api_bind = match api_text.parse::<SocketAddr>() {
            Ok(bind) => {
                if !bind.ip().is_loopback() {
                    warnings.push(format!("[api] bind is {bind}, which can be reached from other machines. The pages are read-only, but put a web server in front of them for the public."));
                }
                Some(bind)
            }
            Err(_) => {
                problem(format!("[api] bind is \"{api_text}\", which is not an address and port. Write it like 127.0.0.1:8788."));
                None
            }
        };

        // ---- stratum
        let s = &raw.stratum;
        let shares_per_min = s.shares_per_min.unwrap_or(DEFAULT_SHARES_PER_MIN);
        if !(1..=600).contains(&shares_per_min) {
            problem(format!("[stratum] shares_per_min is {shares_per_min}, but it must be between 1 and 600."));
        }
        let extranonce_size = s.extranonce_size.unwrap_or(2);
        if extranonce_size > 3 {
            problem(format!("[stratum] extranonce_size is {extranonce_size}, but it must be 0, 1, 2 or 3."));
        }
        let block_wait_ms = s.block_wait_time_ms.unwrap_or(DEFAULT_BLOCK_WAIT_MS);
        if !(50..=60_000).contains(&block_wait_ms) {
            problem(format!("[stratum] block_wait_time_ms is {block_wait_ms}, but it must be between 50 and 60000."));
        }

        let mut ports = Vec::new();
        let mut seen = BTreeSet::new();
        if s.instances.is_empty() {
            problem("[stratum] has no ports. Add at least one [[stratum.instances]] with a port and a min_share_diff.".to_owned());
        }
        for (i, instance) in s.instances.iter().enumerate() {
            let n = i + 1;
            let listen = match listen_address(&instance.port) {
                Ok(listen) => Some(listen),
                Err(why) => {
                    problem(format!("[[stratum.instances]] number {n}: port: {why}."));
                    None
                }
            };
            if !instance.min_share_diff.is_finite() || instance.min_share_diff <= 0.0 {
                problem(format!(
                    "[[stratum.instances]] number {n}: min_share_diff is {}, but it must be a number above zero (0.002 suits a CPU, 2048 an ASIC).",
                    instance.min_share_diff
                ));
            }
            if let Some(listen) = listen {
                if let Some(number) = port_number(&listen)
                    && !seen.insert(number)
                {
                    problem(format!("[[stratum.instances]] number {n}: port {number} is used twice."));
                }
                ports.push(StratumPort { listen, min_share_diff: instance.min_share_diff });
            }
        }

        if !problems.is_empty() {
            return Err(ConfigError(problems));
        }
        Ok(PoolConfig {
            network,
            node_grpc,
            node_wrpc,
            pool_address: pool_address.expect("checked above"),
            fee_bp,
            window_daa,
            min_payout,
            payout_schedule: payout_schedule.expect("checked above"),
            payout_cron,
            data_dir: PathBuf::from(raw.pool.data_dir.trim()),
            api_bind: api_bind.expect("checked above"),
            stratum: StratumConfig {
                shares_per_min,
                var_diff: s.var_diff.unwrap_or(true),
                pow2_clamp: s.pow2_clamp.unwrap_or(true),
                extranonce_size,
                block_wait_time: Duration::from_millis(block_wait_ms),
                print_stats: s.print_stats.unwrap_or(false),
                ports,
            },
            warnings,
        })
    }

    pub fn bridge_settings(&self) -> BridgeSettings {
        BridgeSettings {
            node_grpc: self.node_grpc.clone(),
            pool_address: self.pool_address.to_string(),
            ports: self.stratum.ports.clone(),
            shares_per_min: self.stratum.shares_per_min,
            var_diff: self.stratum.var_diff,
            pow2_clamp: self.stratum.pow2_clamp,
            extranonce_size: self.stratum.extranonce_size,
            block_wait_time: self.stratum.block_wait_time,
            print_stats: self.stratum.print_stats,
        }
    }

    /// The settings in plain words, for `check`.
    pub fn summary<C: Chain>(&self) -> String {
        let ports: Vec<String> =
            self.stratum.ports.iter().map(|p| format!("{} (easiest share {})", p.listen, p.min_share_diff)).collect();
        let mut lines = vec![
            format!("Network: {}", self.network),
            format!("Pool address: {}", self.pool_address),
            format!("Node: {} (stratum and rewards), {} (wallet)", self.node_grpc, self.node_wrpc),
            format!("Stratum ports: {}", ports.join(", ")),
            format!("Pool fee: {}.{:02}%", self.fee_bp / 100, self.fee_bp % 100),
            format!("Miners are paid once owed {}, on the schedule \"{}\"", self.min_payout.display::<C>(), self.payout_cron),
            format!("Share window: {} steps", self.window_daa),
            format!("Pages and metrics: http://{}/", self.api_bind),
            format!("Data directory: {}", self.data_dir.display()),
        ];
        lines.extend(self.warnings.iter().map(|w| format!("Note: {w}")));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::MarigoldChain;
    use pool_chain::testing::FakeChain;
    use pool_domain::UNITS_PER_COIN;

    /// A complete, valid file for the fake chain.
    const GOOD: &str = r#"
[chain]
name = "marigold"

[network]
id = "fakenet"

[pool]
address = "fake:pool"
data_dir = "/var/lib/marigold/pool"

[stratum]

[[stratum.instances]]
port = ":5570"
min_share_diff = 0.002
"#;

    fn problems(text: &str) -> Vec<String> {
        PoolConfig::parse::<FakeChain>(text).expect_err("expected problems").0
    }

    fn with(replace: &str, with: &str) -> String {
        assert!(GOOD.contains(replace), "{replace:?} is not in the base file");
        GOOD.replace(replace, with)
    }

    #[test]
    fn a_minimal_file_gets_the_documented_defaults() {
        let c = PoolConfig::parse::<FakeChain>(GOOD).unwrap();
        assert_eq!(c.network, "fakenet");
        assert_eq!(c.node_grpc, "127.0.0.1:1", "the chain's usual port when none is given");
        assert_eq!(c.node_wrpc, "ws://127.0.0.1:2");
        assert_eq!((c.fee_bp, c.window_daa, c.min_payout.units()), (100, 36_000, 10 * UNITS_PER_COIN));
        assert_eq!(c.payout_cron, "0 5 * * *");
        assert_eq!(c.api_bind, "127.0.0.1:8788".parse().unwrap());
        assert_eq!(
            (c.stratum.shares_per_min, c.stratum.var_diff, c.stratum.pow2_clamp, c.stratum.extranonce_size),
            (30, true, true, 2)
        );
        assert_eq!(c.stratum.block_wait_time, Duration::from_millis(1000));
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn the_settings_become_the_bridges_settings() {
        let c = PoolConfig::parse::<FakeChain>(GOOD).unwrap();
        let b = c.bridge_settings();
        assert_eq!(b.pool_address, "fake:pool");
        assert_eq!(b.node_grpc, "127.0.0.1:1");
        assert_eq!(b.ports, vec![StratumPort { listen: ":5570".into(), min_share_diff: 0.002 }]);
        assert_eq!((b.shares_per_min, b.extranonce_size), (30, 2));
    }

    #[test]
    fn the_minimum_payout_is_named_after_the_chains_ticker_and_may_have_decimals() {
        let c = PoolConfig::parse::<FakeChain>(&with("data_dir", "min_payout_tst = 0.5\ndata_dir")).unwrap();
        assert_eq!(c.min_payout.units(), 50_000_000, "half a coin");
        let c = PoolConfig::parse::<FakeChain>(&with("data_dir", "min_payout_tst = 25\ndata_dir")).unwrap();
        assert_eq!(c.min_payout.units(), 25 * UNITS_PER_COIN);
        let c = PoolConfig::parse::<FakeChain>(&with("data_dir", "min_payout_tst = 0.00000001\ndata_dir")).unwrap();
        assert_eq!(c.min_payout.units(), 1, "a single smallest unit");
    }

    #[test]
    fn the_wrong_chains_minimum_payout_name_is_refused_with_the_right_one() {
        let found = problems(&with("data_dir", "min_payout_magld = 5\ndata_dir"));
        assert_eq!(found, ["[pool] min_payout_magld is not a setting the pool knows. (The minimum payout is called min_payout_tst.)"]);
    }

    #[test]
    fn a_bad_minimum_payout_is_explained() {
        for bad in ["0", "-3", "\"ten\"", "1e20", "true"] {
            let found = problems(&with("data_dir", &format!("min_payout_tst = {bad}\ndata_dir")));
            assert_eq!(found.len(), 1, "{bad}: {found:?}");
            assert!(found[0].contains("min_payout_tst must be a number of coins above zero"), "{bad}: {found:?}");
        }
    }

    #[test]
    fn every_problem_is_reported_at_once_and_names_its_key() {
        let found = problems(
            &GOOD
                .replace("name = \"marigold\"", "name = \"kaspa\"")
                .replace("id = \"fakenet\"", "id = \"elsewhere\"")
                .replace("address = \"fake:pool\"", "address = \"\"")
                .replace(
                    "data_dir = \"/var/lib/marigold/pool\"",
                    "data_dir = \"\"\nfee_bp = 20000\nwindow_daa = 5\npayout_cron = \"sometimes\"",
                )
                .replace("min_share_diff = 0.002", "min_share_diff = 0"),
        );
        let all = found.join("\n");
        for needle in [
            "[chain] name is \"kaspa\"",
            "[network] id is \"elsewhere\"",
            "[pool] address is empty",
            "[pool] data_dir is empty",
            "[pool] fee_bp is 20000",
            "[pool] window_daa is 5",
            "[pool] payout_cron",
            "min_share_diff is 0",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
    }

    #[test]
    fn an_empty_address_tells_the_operator_to_run_init() {
        assert_eq!(
            problems(&with("address = \"fake:pool\"", "address = \"\"")),
            ["[pool] address is empty. Run `marigold-pool init`, then put the address it prints here."]
        );
    }

    #[test]
    fn an_address_from_the_wrong_network_is_refused_in_the_chains_words() {
        let found = problems(&with("address = \"fake:pool\"", "address = \"nonsense\""));
        assert_eq!(found, ["[pool] address is not usable: That does not look like an address. It starts with fake:."]);
    }

    #[test]
    fn an_unknown_key_is_refused_by_name_and_line() {
        let found = problems(&with("[stratum]\n", "[stratum]\nshares_per_minute = 30\n"));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("unknown field `shares_per_minute`") && found[0].contains("line"), "{found:?}");
        let found = problems(&format!("{GOOD}\n[extras]\nx = 1\n"));
        assert!(found[0].contains("unknown field `extras`"), "{found:?}");
    }

    #[test]
    fn a_file_that_is_not_toml_is_refused() {
        let found = problems("this is not [valid");
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("The settings could not be read:"), "{found:?}");
    }

    #[test]
    fn a_missing_required_section_is_named() {
        let found = problems(&GOOD.replace("[chain]\nname = \"marigold\"\n", ""));
        assert!(found[0].contains("missing field `chain`"), "{found:?}");
    }

    #[test]
    fn the_fee_may_be_nothing_or_everything_and_no_more() {
        assert_eq!(PoolConfig::parse::<FakeChain>(&with("data_dir", "fee_bp = 0\ndata_dir")).unwrap().fee_bp, 0);
        assert_eq!(PoolConfig::parse::<FakeChain>(&with("data_dir", "fee_bp = 10000\ndata_dir")).unwrap().fee_bp, 10_000);
        assert!(problems(&with("data_dir", "fee_bp = 10001\ndata_dir"))[0].contains("cannot be more than 10000"));
        assert!(
            problems(&with("data_dir", "fee_bp = 70000\ndata_dir"))[0].contains("[pool] fee_bp is 70000"),
            "bigger than a u16 too"
        );
    }

    #[test]
    fn the_window_has_sensible_limits() {
        assert!(PoolConfig::parse::<FakeChain>(&with("data_dir", "window_daa = 1000\ndata_dir")).is_ok());
        assert!(PoolConfig::parse::<FakeChain>(&with("data_dir", "window_daa = 10000000\ndata_dir")).is_ok());
        assert!(problems(&with("data_dir", "window_daa = 999\ndata_dir"))[0].contains("window_daa is 999"));
        assert!(problems(&with("data_dir", "window_daa = 10000001\ndata_dir"))[0].contains("window_daa is 10000001"));
    }

    #[test]
    fn the_nodes_addresses_are_checked_and_may_carry_a_scheme() {
        let c = PoolConfig::parse::<FakeChain>(&with(
            "[pool]",
            "[node]\ngrpc = \"grpc://10.0.0.5:26210\"\nwrpc = \"wss://node.example:27210\"\n\n[pool]",
        ))
        .unwrap();
        assert_eq!((c.node_grpc.as_str(), c.node_wrpc.as_str()), ("10.0.0.5:26210", "wss://node.example:27210"));
        let found = problems(&with("[pool]", "[node]\ngrpc = \"localhost\"\nwrpc = \"http://x\"\n\n[pool]"));
        assert!(found.iter().any(|f| f.contains("[node] grpc is \"localhost\"")), "{found:?}");
        assert!(found.iter().any(|f| f.contains("[node] wrpc is \"http://x\"")), "{found:?}");
    }

    #[test]
    fn the_api_address_is_checked_and_a_public_one_gets_a_note() {
        let c = PoolConfig::parse::<FakeChain>(&with("[stratum]\n", "[api]\nbind = \"0.0.0.0:8788\"\n\n[stratum]\n")).unwrap();
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("can be reached from other machines"), "{:?}", c.warnings);
        assert!(
            problems(&with("[stratum]\n", "[api]\nbind = \"everywhere\"\n\n[stratum]\n"))[0].contains("[api] bind is \"everywhere\"")
        );
    }

    #[test]
    fn stratum_ports_may_be_numbers_or_text_and_must_differ() {
        let two = format!(
            "{}\n[[stratum.instances]]\nport = 5571\nmin_share_diff = 2048\n\n[[stratum.instances]]\nport = \"127.0.0.1:5572\"\nmin_share_diff = 1\n",
            GOOD
        );
        let c = PoolConfig::parse::<FakeChain>(&two).unwrap();
        assert_eq!(
            c.stratum.ports,
            vec![
                StratumPort { listen: ":5570".into(), min_share_diff: 0.002 },
                StratumPort { listen: ":5571".into(), min_share_diff: 2048.0 },
                StratumPort { listen: "127.0.0.1:5572".into(), min_share_diff: 1.0 },
            ]
        );
        let twice = format!("{GOOD}\n[[stratum.instances]]\nport = \"5570\"\nmin_share_diff = 1\n");
        assert_eq!(problems(&twice), ["[[stratum.instances]] number 2: port 5570 is used twice."]);
        let bad = format!("{GOOD}\n[[stratum.instances]]\nport = 70000\nmin_share_diff = 1\n");
        assert!(problems(&bad)[0].contains("number 2: port: 70000 is not a port number"));
        let worse = format!("{GOOD}\n[[stratum.instances]]\nport = \"banana\"\nmin_share_diff = 1\n");
        assert!(problems(&worse)[0].contains("\"banana\" is not a port"));
    }

    #[test]
    fn at_least_one_port_is_needed() {
        let none = GOOD.replace("[[stratum.instances]]\nport = \":5570\"\nmin_share_diff = 0.002\n", "");
        assert!(problems(&none)[0].contains("[stratum] has no ports"));
    }

    #[test]
    fn the_share_difficulty_must_be_above_zero_and_may_be_a_fraction_or_a_whole_number() {
        let c =
            PoolConfig::parse::<FakeChain>(&format!("{GOOD}\n[[stratum.instances]]\nport = 5571\nmin_share_diff = 2048\n")).unwrap();
        assert_eq!(c.stratum.ports[1].min_share_diff, 2048.0);
        for bad in ["0", "-1", "nan"] {
            let text = GOOD.replace("min_share_diff = 0.002", &format!("min_share_diff = {bad}"));
            assert!(problems(&text)[0].contains("min_share_diff"), "{bad}");
        }
    }

    #[test]
    fn the_stratum_tuning_is_range_checked() {
        for (line, needle) in [
            ("shares_per_min = 0", "shares_per_min is 0"),
            ("shares_per_min = 601", "shares_per_min is 601"),
            ("extranonce_size = 4", "extranonce_size is 4"),
            ("block_wait_time_ms = 10", "block_wait_time_ms is 10"),
        ] {
            let found = problems(&with("[stratum]\n", &format!("[stratum]\n{line}\n")));
            assert!(found[0].contains(needle), "{line}: {found:?}");
        }
    }

    // ---- the real file we ship ---------------------------------------------------------

    const SHIPPED: &str = include_str!("../../../deploy/pool.toml");

    #[test]
    fn the_shipped_file_is_refused_until_the_operator_has_run_init() {
        let found = PoolConfig::parse::<MarigoldChain>(SHIPPED).expect_err("the address is blank on purpose").0;
        assert_eq!(found, ["[pool] address is empty. Run `marigold-pool init`, then put the address it prints here."]);
    }

    #[test]
    fn the_shipped_file_is_complete_once_the_address_is_filled_in() {
        let filled = SHIPPED.replacen(
            "address = \"\"",
            "address = \"marigoldtest:qqfc69eulu3v8qamcasxqcx3wxvsfzjkv73fau4exk5kem70nqjsqgm3wm9g8\"",
            1,
        );
        let c = PoolConfig::parse::<MarigoldChain>(&filled).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.network, "testnet-10");
        assert_eq!((c.node_grpc.as_str(), c.node_wrpc.as_str()), ("127.0.0.1:26210", "ws://127.0.0.1:27210"));
        assert_eq!(c.data_dir, PathBuf::from("/var/lib/marigold/pool"));
        assert_eq!(c.api_bind, "127.0.0.1:8788".parse().unwrap());
        assert_eq!((c.fee_bp, c.window_daa, c.min_payout.units()), (100, 36_000, 10 * UNITS_PER_COIN));
        assert_eq!(c.stratum.shares_per_min, 30);
        assert_eq!(
            c.stratum.ports,
            vec![
                StratumPort { listen: ":5570".into(), min_share_diff: 0.002 },
                StratumPort { listen: ":5571".into(), min_share_diff: 2048.0 },
            ],
            "the CPU port takes a fraction, the ASIC port keeps 2048"
        );
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn the_summary_says_what_the_pool_will_do() {
        let filled = SHIPPED.replacen(
            "address = \"\"",
            "address = \"marigoldtest:qqfc69eulu3v8qamcasxqcx3wxvsfzjkv73fau4exk5kem70nqjsqgm3wm9g8\"",
            1,
        );
        let summary = PoolConfig::parse::<MarigoldChain>(&filled).unwrap().summary::<MarigoldChain>();
        for needle in [
            "Network: testnet-10",
            "Stratum ports: :5570 (easiest share 0.002), :5571 (easiest share 2048)",
            "Pool fee: 1.00%",
            "once owed 10.00 MAGLD",
            "Pages and metrics: http://127.0.0.1:8788/",
        ] {
            assert!(summary.contains(needle), "missing {needle:?} in:\n{summary}");
        }
    }
}
