//! `marigold-pool rewards`: look at the block rewards that have reached an
//! address, straight from the node. It writes nothing anywhere.

use clap::Args;
use pool_accountant::is_mature;
use pool_chain::{Chain, NodeUtxo};
use pool_domain::{Amount, MinerAddress};

#[derive(Args)]
pub struct RewardsArgs {
    /// The address block rewards are paid to.
    #[arg(long)]
    pub address: String,
    /// The network the address is on.
    #[arg(long, default_value = "testnet-10")]
    pub network: String,
    /// The node to ask, such as grpc://127.0.0.1:26210. By default the node on this machine, on the network's usual port.
    #[arg(long)]
    pub node: Option<String>,
}

/// Ask the node and return the text to print, or a sentence saying what is wrong.
pub async fn run<C: Chain>(args: RewardsArgs) -> Result<String, String> {
    let address = MinerAddress::parse::<C>(&args.network, &args.address).map_err(|e| e.to_string())?;
    let port = C::default_grpc_port(&args.network).ok_or_else(|| unknown_network(&args.network))?;
    let maturity = C::coinbase_maturity_daa(&args.network).ok_or_else(|| unknown_network(&args.network))?;
    let url = args.node.unwrap_or_else(|| format!("grpc://127.0.0.1:{port}"));

    let node = C::connect_node(&url).await.map_err(|e| e.to_string())?;
    let virtual_daa = node.virtual_daa_score().await.map_err(|e| e.to_string())?;
    let utxos = node.utxos_at(address.as_str()).await.map_err(|e| e.to_string())?;
    // A second, independent question, so the list can be checked against the
    // node's own total. The two answers are not taken at the same instant.
    let node_total = node.balance_at(address.as_str()).await.map_err(|e| e.to_string())?;
    Ok(render::<C>(address.as_str(), &args.network, virtual_daa, maturity, &utxos, node_total))
}

fn unknown_network(network: &str) -> String {
    format!("There is no network called \"{network}\". Use mainnet or testnet-10.")
}

/// The report, as plain lines. Rewards are listed oldest first; the totals give
/// the exact amount in the smallest unit so they can be compared to a wallet.
pub fn render<C: Chain>(address: &str, network: &str, virtual_daa: u64, maturity: u64, utxos: &[NodeUtxo], node_total: u64) -> String {
    let mut rewards: Vec<&NodeUtxo> = utxos.iter().filter(|u| u.is_coinbase).collect();
    rewards.sort_by(|a, b| (a.block_daa_score, &a.outpoint).cmp(&(b.block_daa_score, &b.outpoint)));

    let mut lines = vec![
        format!("Block rewards at {address} on {network}"),
        format!(
            "The node is at DAA score {virtual_daa}. A reward can be split once {maturity} steps have passed since it was created."
        ),
        String::new(),
    ];

    let (mut ready_total, mut ready_count) = (0u64, 0u64);
    let (mut waiting_total, mut waiting_count) = (0u64, 0u64);
    for reward in &rewards {
        let waited = virtual_daa.saturating_sub(reward.block_daa_score);
        let status = if is_mature(reward.block_daa_score, virtual_daa, maturity) {
            ready_total += reward.amount;
            ready_count += 1;
            format!("ready (waited {waited})")
        } else {
            waiting_total += reward.amount;
            waiting_count += 1;
            format!("waiting, {} more steps", maturity - waited)
        };
        lines.push(format!(
            "{}  {}  created at {}  {}",
            reward.outpoint,
            Amount::new(reward.amount).display::<C>(),
            reward.block_daa_score,
            status
        ));
    }
    if rewards.is_empty() {
        lines.push("No block rewards have arrived at this address.".to_owned());
    }

    let (mut other_total, mut other_count) = (0u64, 0u64);
    for payment in utxos.iter().filter(|u| !u.is_coinbase) {
        other_total += payment.amount;
        other_count += 1;
    }

    lines.push(String::new());
    lines.push(format!(
        "Ready:    {} in {} reward(s), {} {}s",
        Amount::new(ready_total).display::<C>(),
        ready_count,
        ready_total,
        C::unit_name()
    ));
    lines.push(format!(
        "Waiting:  {} in {} reward(s), {} {}s",
        Amount::new(waiting_total).display::<C>(),
        waiting_count,
        waiting_total,
        C::unit_name()
    ));
    lines.push(format!(
        "Ordinary payments (not rewards): {} in {} output(s), {} {}s",
        Amount::new(other_total).display::<C>(),
        other_count,
        other_total,
        C::unit_name()
    ));
    let listed = ready_total.saturating_add(waiting_total).saturating_add(other_total);
    lines.push(String::new());
    if listed == node_total {
        lines.push(format!(
            "Check: the node counts {node_total} {}s at this address, which is exactly what is listed.",
            C::unit_name()
        ));
    } else {
        lines.push(format!(
            "Check: the node counts {node_total} {}s at this address but {listed} are listed. Outputs may have arrived or been spent between the two questions; ask again.",
            C::unit_name()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::MarigoldChain;

    fn utxo(outpoint: &str, amount: u64, block_daa_score: u64, is_coinbase: bool) -> NodeUtxo {
        NodeUtxo { outpoint: outpoint.into(), amount, block_daa_score, is_coinbase }
    }

    #[test]
    fn the_report_lists_rewards_oldest_first_with_their_state_and_exact_totals() {
        let utxos = vec![
            utxo("bb:0", 2_000_000_000, 1_500, true), // waited 500: waiting, 500 more
            utxo("aa:1", 1_234_000_000, 500, true),   // waited 1500: ready
            utxo("cc:0", 77_000_000, 900, false),     // an ordinary payment
        ];
        let text = render::<MarigoldChain>("marigoldtest:qq", "testnet-10", 2_000, 1_000, &utxos, 3_311_000_000);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines[0], "Block rewards at marigoldtest:qq on testnet-10");
        assert!(lines[1].contains("DAA score 2000") && lines[1].contains("1000 steps"), "{}", lines[1]);
        assert_eq!(lines[3], "aa:1  12.34 MAGLD  created at 500  ready (waited 1500)");
        assert_eq!(lines[4], "bb:0  20.00 MAGLD  created at 1500  waiting, 500 more steps");
        assert!(text.contains("Ready:    12.34 MAGLD in 1 reward(s), 1234000000 petals"), "{text}");
        assert!(text.contains("Waiting:  20.00 MAGLD in 1 reward(s), 2000000000 petals"), "{text}");
        assert!(text.contains("Ordinary payments (not rewards): 0.77 MAGLD in 1 output(s), 77000000 petals"), "{text}");
    }

    #[test]
    fn the_check_line_agrees_when_the_totals_match_and_says_what_differs_when_they_do_not() {
        let utxos = vec![utxo("aa:0", 100, 10, true), utxo("bb:0", 50, 20, false)];
        let agree = render::<MarigoldChain>("a", "testnet-10", 5_000, 1_000, &utxos, 150);
        assert!(agree.contains("Check: the node counts 150 petals at this address, which is exactly what is listed."), "{agree}");
        let differ = render::<MarigoldChain>("a", "testnet-10", 5_000, 1_000, &utxos, 175);
        assert!(differ.contains("the node counts 175 petals at this address but 150 are listed."), "{differ}");
    }

    #[test]
    fn an_address_with_no_rewards_says_so() {
        let text = render::<MarigoldChain>("marigoldtest:qq", "testnet-10", 10, 1_000, &[], 0);
        assert!(text.contains("No block rewards have arrived at this address."), "{text}");
        assert!(text.contains("Ready:    0.00 MAGLD in 0 reward(s), 0 petals"), "{text}");
    }

    #[test]
    fn a_reward_exactly_at_the_limit_is_ready_and_one_step_short_is_waiting() {
        let utxos = vec![utxo("at:0", 1, 1_000, true), utxo("short:0", 1, 1_001, true)];
        let text = render::<MarigoldChain>("a", "testnet-10", 2_000, 1_000, &utxos, 2);
        assert!(text.contains("at:0  0.00 MAGLD  created at 1000  ready (waited 1000)"), "{text}");
        assert!(text.contains("short:0  0.00 MAGLD  created at 1001  waiting, 1 more steps"), "{text}");
    }

    #[tokio::test]
    async fn a_bad_address_is_refused_in_words_before_any_node_is_contacted() {
        let args = RewardsArgs { address: "nonsense".into(), network: "testnet-10".into(), node: Some("grpc://127.0.0.1:1".into()) };
        let error = run::<MarigoldChain>(args).await.unwrap_err();
        assert_eq!(error, "That does not look like a Marigold address. It starts with marigoldtest: on the test network.");
    }

    #[tokio::test]
    async fn an_unknown_network_is_refused_in_words() {
        let args = RewardsArgs { address: "x".into(), network: "elsewhere".into(), node: None };
        let error = run::<MarigoldChain>(args).await.unwrap_err();
        assert!(error.contains("elsewhere") && error.contains("testnet-10"), "{error}");
    }
}
