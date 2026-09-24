//! Command line: account maintenance and `serve`.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::{broadcast, mpsc};

use crate::app::{App, restore};
use crate::broker::SimBroker;
use crate::domain::{Clock, Currency, SystemClock};
use crate::feed::binance::BinanceFeed;
use crate::feed::upbit::UpbitFeed;
use crate::feed::{MarketFeed, run_feed};
use crate::fx::FxCache;
use crate::market::{Market, pump};
use crate::persist::persist;
use crate::store::Store;
use crate::tools::{TraderServer, TraderTools};
use crate::venue::Calendar;

const USAGE: &str = "atrader — paper trading for Attacca agents

USAGE:
  atrader serve [--no-zyris]
  atrader account create <id> <name> [--agent <attacca-agent-id>] [--cash KRW=10000000]...
  atrader account list
  atrader account reset <id> [--cash KRW=10000000]...
  atrader user create <username>
  atrader user reset-2fa <username>

ENVIRONMENT:
  DATABASE_URL          Postgres connection string (required)
  ZYRIS_CREDENTIAL      zc_ credential issued in Attacca (/settings/zyris), or
  ZYRIS_CREDENTIAL_FILE file holding it
  ZYRIS_SERVER_URL      zyris server (default: Attacca's)
  ATRADER_NODE_NAME     node name shown in Attacca (default: atrader)
  KIS_APP_KEY, KIS_APP_SECRET  KIS Open API keys (enable KRX and US stocks)
  KIS_ENV               `mock` for KIS mock-trading hosts (default: real)
  ATRADER_STATE_DIR     where the KIS token is cached (default: ~/.local/state/atrader)
  DART_API_KEY          OpenDART key (KRX financials and filings)
  EDGAR_USER_AGENT      name and contact email for SEC EDGAR (US financials and filings)
  RUST_LOG              log filter (default: atrader=info,zyris_core=info)";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Serve { zyris: bool },
    AccountCreate { id: String, name: String, agent: Option<String>, cash: Vec<(Currency, Decimal)> },
    AccountList,
    AccountReset { id: String, cash: Vec<(Currency, Decimal)> },
    UserCreate { username: String },
    UserReset2fa { username: String },
    Version,
    Help,
}

pub fn default_cash() -> Vec<(Currency, Decimal)> {
    vec![(Currency::Krw, dec!(10000000)), (Currency::Usd, dec!(7000)), (Currency::Usdt, dec!(7000))]
}

fn parse_cash(s: &str) -> Result<(Currency, Decimal), String> {
    let (c, amount) = s.split_once('=').ok_or_else(|| format!("--cash wants CURRENCY=AMOUNT, got {s:?}"))?;
    let c = Currency::from_code(&c.to_uppercase()).ok_or_else(|| format!("unknown currency {c:?}; use KRW, USD or USDT"))?;
    let amount: Decimal = amount.parse().map_err(|_| format!("bad amount {amount:?}"))?;
    if amount < Decimal::ZERO {
        return Err("cash cannot be negative".into());
    }
    Ok((c, amount))
}

/// `--agent X` and repeated `--cash C=N` after the positional arguments.
fn parse_flags(rest: &[String]) -> Result<(Option<String>, Vec<(Currency, Decimal)>), String> {
    let (mut agent, mut cash) = (None, Vec::new());
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--agent" => agent = Some(value.clone()),
            "--cash" => cash.push(parse_cash(value)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok((agent, if cash.is_empty() { default_cash() } else { cash }))
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["help"] | ["--help"] | ["-h"] => Ok(Command::Help),
        ["--version"] | ["-V"] => Ok(Command::Version),
        ["serve"] => Ok(Command::Serve { zyris: true }),
        ["serve", "--no-zyris"] => Ok(Command::Serve { zyris: false }),
        ["account", "list"] => Ok(Command::AccountList),
        ["account", "create", id, name, ..] => {
            let (agent, cash) = parse_flags(&args[4..])?;
            Ok(Command::AccountCreate { id: id.to_string(), name: name.to_string(), agent, cash })
        }
        ["user", "create", name] => Ok(Command::UserCreate { username: name.to_string() }),
        ["user", "reset-2fa", name] => Ok(Command::UserReset2fa { username: name.to_string() }),
        ["account", "reset", id, ..] => {
            let (agent, cash) = parse_flags(&args[3..])?;
            if agent.is_some() {
                return Err("reset does not take --agent".into());
            }
            Ok(Command::AccountReset { id: id.to_string(), cash })
        }
        _ => Err(format!("unrecognised command: {}\n\n{USAGE}", words.join(" "))),
    }
}

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let command = parse(&args).map_err(|e| anyhow!(e))?;
    match command {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Version => {
            println!("atrader {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "atrader=info,zyris_core=info".into()),
        )
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
        let store = Arc::new(Store::connect(&url).await?);
        match command {
            Command::AccountCreate { id, name, agent, cash } => {
                store.create_account(&id, &name, agent.as_deref(), &cash, chrono::Utc::now()).await?;
                println!("created account {id}");
            }
            Command::AccountList => {
                for a in store.list_accounts().await? {
                    let cash = store.cash_balances(&a.id, a.generation).await?;
                    let cash: Vec<String> = cash.iter().map(|(c, v)| format!("{}={v}", c.code())).collect();
                    println!(
                        "{:<16} {:<24} agent={:<24} gen={} {}",
                        a.id,
                        a.name,
                        a.agent_id.as_deref().unwrap_or("-"),
                        a.generation,
                        cash.join(" ")
                    );
                }
            }
            Command::UserCreate { username } => {
                let pw = rpassword::prompt_password("password (12+ characters): ")?;
                if pw.chars().count() < 12 {
                    anyhow::bail!("the password must be at least 12 characters");
                }
                if rpassword::prompt_password("again: ")? != pw {
                    anyhow::bail!("the passwords differ");
                }
                crate::web::auth::AuthStore(store.pool().clone()).create_user(&username, &crate::web::auth::hash_password(&pw)).await?;
                println!("created user {username}; log in to the dashboard to enrol two-factor authentication");
            }
            Command::UserReset2fa { username } => {
                let auth = crate::web::auth::AuthStore(store.pool().clone());
                let user = auth.user_by_name(&username).await?.ok_or_else(|| anyhow!("no user {username}"))?;
                auth.set_totp(user.id, None, false).await?;
                auth.save_recovery_codes(user.id, &[]).await?;
                auth.delete_user_sessions(user.id).await?;
                println!("two-factor authentication cleared for {username}; it is enrolled again at the next login");
            }
            Command::AccountReset { id, cash } => {
                // A running server keeps trading the old generation in memory; stop it first.
                let generation = store.reset_account(&id, &cash, chrono::Utc::now()).await?;
                println!("account {id} reset (generation {generation}); start `atrader serve` again to trade it");
            }
            Command::Serve { zyris } => serve(store, zyris).await?,
            Command::Help | Command::Version => unreachable!("handled above"),
        }
        Ok(())
    })
}

fn credential() -> anyhow::Result<String> {
    if let Ok(c) = std::env::var("ZYRIS_CREDENTIAL") {
        if !c.trim().is_empty() {
            return Ok(c.trim().to_string());
        }
    }
    if let Ok(path) = std::env::var("ZYRIS_CREDENTIAL_FILE") {
        let c = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        return Ok(c.trim().to_string());
    }
    Err(anyhow!(
        "no zyris credential: set ZYRIS_CREDENTIAL (issue one in Attacca under /settings/zyris) or run `atrader serve --no-zyris`"
    ))
}

async fn serve(store: Arc<Store>, with_zyris: bool) -> anyhow::Result<()> {
    crate::init_tls();
    let token = if with_zyris { Some(credential()?) } else { None };
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let calendar = Calendar::from_toml(include_str!("../holidays.toml"))?;
    let (journal_tx, journal_rx) = mpsc::unbounded_channel();
    let broker = Arc::new(SimBroker::new(clock.clone(), calendar).with_journal(journal_tx));
    let (bus, _) = broadcast::channel(1024);
    let mut market = Market::new(broker.clone());

    let (events_tx, events_rx) = mpsc::channel(4096);
    let mut feeds: Vec<(Arc<dyn MarketFeed>, usize)> =
        vec![(Arc::new(UpbitFeed::new(clock.clone())), 50), (Arc::new(BinanceFeed::new(clock.clone())), 100)];
    match crate::feed::kis::KisConfig::from_env() {
        Some(cfg) => {
            let client = Arc::new(crate::feed::kis::KisClient::new(cfg));
            let calendar = Calendar::from_toml(include_str!("../holidays.toml"))?;
            // 41 real-time registrations per appkey; book + trade = 2 per instrument.
            feeds.push((Arc::new(crate::feed::kis::KisKrxFeed::new(client.clone(), clock.clone(), calendar.clone())), 10));
            feeds.push((Arc::new(crate::feed::kis::KisUsFeed::new(client, clock.clone(), calendar)), 10));
        }
        None => tracing::info!("KIS_APP_KEY/KIS_APP_SECRET not set; KRX and US stocks are disabled"),
    }

    for (feed, cap) in feeds {
        let subs = market.add_feed(feed.clone(), cap);
        tokio::spawn(run_feed(feed, subs, events_tx.clone()));
    }
    drop(events_tx);
    let instruments = market.load_instruments().await?;
    let accounts = restore(&store, &broker).await?;
    let slot = crate::alerts::deliver::ConnSlot::default();
    let (alert_tx, alert_rx) = mpsc::unbounded_channel();
    tracing::info!(instruments, accounts, "state restored");

    tokio::spawn(pump(events_rx, broker.clone(), bus.clone()));
    let writer = tokio::spawn(persist(journal_rx, store.clone(), bus.clone()));
    tokio::spawn(crate::app::bar_loop(bus.subscribe(), store.clone()));
    let dart = std::env::var("DART_API_KEY").ok().filter(|k| !k.trim().is_empty()).map(|k| crate::fundamentals::dart::DartClient::new(k.trim().into()));
    let edgar = std::env::var("EDGAR_USER_AGENT").ok().filter(|u| !u.trim().is_empty()).map(|u| crate::fundamentals::edgar::EdgarClient::new(u.trim().into()));
    tracing::info!(dart = dart.is_some(), edgar = edgar.is_some(), "fundamentals sources");
    let app = Arc::new(App::new(broker.clone(), store, market, FxCache::new()).await?.with_fundamentals(dart, edgar).with_alerts(alert_tx));
    app.market.refresh_pins();
    tokio::spawn(crate::alerts::deliver::alert_loop(app.clone(), bus.subscribe(), alert_rx, Arc::new(crate::alerts::deliver::AttaccaNotifier::new(slot.clone()))));
    tokio::spawn(crate::app::snapshot_loop(app.clone()));

    let timers = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            timers.market.refresh_pins();
            timers.broker.expire_day_orders();
        }
    });

    let Some(token) = token else {
        tracing::info!("running without zyris; Ctrl-C or SIGTERM to stop");
        shutdown_signal().await?;
        return drain(&broker, writer).await;
    };
    let server = std::env::var("ZYRIS_SERVER_URL").unwrap_or_else(|_| zyris::DEFAULT_SERVER_URL.to_string());
    let name = std::env::var("ATRADER_NODE_NAME").unwrap_or_else(|_| "atrader".into());
    let link = zyris::Node::builder()
        .name(name)
        .kind(zyris::NodeKind::Service)
        .capability(TraderServer(TraderTools::new(app)))
        .on_connect({
            let slot = slot.clone();
            move |conn| {
                let slot = slot.clone();
                async move { slot.put(conn) }
            }
        })
        .build()?
        .connect(&server, &token)
        .await?;
    tracing::info!(node = %link.node_id(), %server, "serving trader capability");
    tokio::select! {
        closed = link.wait_closed() => {
            if let Err(e) = closed {
                drain(&broker, writer).await?;
                return Err(e.into());
            }
        }
        signal = shutdown_signal() => {
            signal?;
            link.disconnect().await;
        }
    }
    drain(&broker, writer).await
}

/// Ctrl-C or SIGTERM (what systemd sends on stop).
async fn shutdown_signal() -> anyhow::Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {}
    }
    Ok(())
}

/// Stop journaling and wait for everything already queued to reach the database.
async fn drain(broker: &SimBroker, writer: tokio::task::JoinHandle<()>) -> anyhow::Result<()> {
    tracing::info!("shutting down; flushing the journal");
    broker.close_journal();
    tokio::time::timeout(std::time::Duration::from_secs(120), writer)
        .await
        .context("journal did not drain within 120 s")??;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&args(&["serve"])), Ok(Command::Serve { zyris: true }));
        assert_eq!(parse(&args(&["serve", "--no-zyris"])), Ok(Command::Serve { zyris: false }));
        assert_eq!(parse(&args(&["account", "list"])), Ok(Command::AccountList));
        assert_eq!(
            parse(&args(&["account", "create", "bot", "My Bot", "--agent", "ag1", "--cash", "KRW=10000000", "--cash", "usd=1000"])),
            Ok(Command::AccountCreate {
                id: "bot".into(),
                name: "My Bot".into(),
                agent: Some("ag1".into()),
                cash: vec![(Currency::Krw, dec!(10000000)), (Currency::Usd, dec!(1000))],
            })
        );
        assert_eq!(
            parse(&args(&["account", "reset", "bot"])),
            Ok(Command::AccountReset { id: "bot".into(), cash: default_cash() })
        );
        assert_eq!(parse(&args(&[])), Ok(Command::Help));
        assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
    }

    #[test]
    fn parses_user_commands() {
        assert_eq!(parse(&args(&["user", "create", "ruma"])), Ok(Command::UserCreate { username: "ruma".into() }));
        assert_eq!(parse(&args(&["user", "reset-2fa", "ruma"])), Ok(Command::UserReset2fa { username: "ruma".into() }));
        assert!(parse(&args(&["user", "create"])).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&["trade"])).is_err());
        assert!(parse(&args(&["account", "create", "bot"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "KRW10"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "EUR=5"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "KRW=-5"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--agent"])).is_err());
        assert!(parse(&args(&["serve", "--fast"])).is_err());
    }
}
