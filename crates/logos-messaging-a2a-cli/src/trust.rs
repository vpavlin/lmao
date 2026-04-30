//! `lmao trust` subcommand handlers.
//!
//! Mutations probe the daemon socket first — if a `lmao agent run` is
//! listening, the request is routed via IPC so the change takes effect
//! in the live agent immediately AND is persisted by the daemon to its
//! trust file. If no daemon is running, the CLI falls back to editing
//! the TOML file directly (same path the daemon would load at next
//! startup).

use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_core::{TrustEntry, TrustList, TrustMode};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cli::TrustAction;
use crate::daemon::{default_socket_path, DaemonClient, Request, Response};

pub async fn handle(
    action: TrustAction,
    trust_file: Option<PathBuf>,
    daemon_socket: Option<PathBuf>,
    keyfile: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let path = trust_file.unwrap_or_else(TrustList::default_path);
    let socket = daemon_socket.unwrap_or_else(default_socket_path);
    let client = DaemonClient::new(socket);
    let daemon_up = client.probe().await;

    match action {
        TrustAction::List => {
            if daemon_up {
                list_via_daemon(&client, json).await
            } else {
                list(&path, json)
            }
        }
        TrustAction::Add {
            pubkey,
            nickname,
            capabilities,
            notes,
        } => {
            if daemon_up {
                add_via_daemon(&client, pubkey, nickname, capabilities, notes, json).await
            } else {
                add(&path, pubkey, nickname, capabilities, notes, json)
            }
        }
        TrustAction::Remove { target } => {
            if daemon_up {
                remove_via_daemon(&client, &target, json).await
            } else {
                remove(&path, &target, json)
            }
        }
        TrustAction::Mode { new_mode } => {
            if daemon_up {
                mode_via_daemon(&client, new_mode.as_deref(), json).await
            } else {
                mode(&path, new_mode.as_deref(), json)
            }
        }
        TrustAction::Import { path: src } => import(&path, &src, json),
        TrustAction::Export => export(&path),
        TrustAction::Pubkey => pubkey(keyfile.as_deref(), json),
    }
}

async fn list_via_daemon(client: &DaemonClient, json: bool) -> Result<()> {
    let resp = client.send(Request::TrustList).await?;
    let Response::TrustList {
        mode,
        entries,
        trust_file,
    } = resp
    else {
        anyhow::bail!("unexpected response from daemon: {resp:?}");
    };
    if json {
        let body = serde_json::json!({
            "source": "daemon",
            "mode": mode,
            "trust_file": trust_file,
            "count": entries.len(),
            "peers": entries,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    eprintln!("Source: daemon");
    if let Some(p) = trust_file {
        eprintln!("trust file: {}", p.display());
    }
    eprintln!("mode:       {mode}");
    eprintln!("entries:    {}", entries.len());
    for e in entries {
        let caps = if e.capabilities.is_empty() {
            "(any)".to_string()
        } else {
            e.capabilities.join(", ")
        };
        let notes = e.notes.as_deref().unwrap_or("");
        println!(
            "  {}  {:<24} caps=[{}] {}",
            &e.pubkey[..16.min(e.pubkey.len())],
            e.nickname,
            caps,
            notes
        );
    }
    Ok(())
}

async fn add_via_daemon(
    client: &DaemonClient,
    pubkey: String,
    nickname: String,
    capabilities: Vec<String>,
    notes: Option<String>,
    json: bool,
) -> Result<()> {
    let resp = client
        .send(Request::TrustAdd {
            pubkey: pubkey.clone(),
            nickname: nickname.clone(),
            capabilities,
            notes,
        })
        .await?;
    let Response::TrustAdd {
        pubkey,
        nickname,
        persisted,
    } = resp
    else {
        anyhow::bail!("unexpected response from daemon: {resp:?}");
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "source": "daemon",
                "added": pubkey,
                "nickname": nickname,
                "persisted": persisted,
            })
        );
    } else {
        eprintln!(
            "added {nickname} ({pubkey}) — agent applied; {}",
            if persisted {
                "trust file updated"
            } else {
                "no trust file configured (in-memory only)"
            }
        );
    }
    Ok(())
}

async fn remove_via_daemon(client: &DaemonClient, target: &str, json: bool) -> Result<()> {
    let resp = client
        .send(Request::TrustRemove {
            target: target.into(),
        })
        .await?;
    let Response::TrustRemove {
        pubkey,
        nickname,
        persisted,
    } = resp
    else {
        anyhow::bail!("unexpected response from daemon: {resp:?}");
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "source": "daemon",
                "removed": pubkey,
                "nickname": nickname,
                "persisted": persisted,
            })
        );
    } else {
        eprintln!(
            "removed {nickname} ({pubkey}) — agent applied; {}",
            if persisted {
                "trust file updated"
            } else {
                "no trust file configured (in-memory only)"
            }
        );
    }
    Ok(())
}

async fn mode_via_daemon(client: &DaemonClient, new_mode: Option<&str>, json: bool) -> Result<()> {
    let resp = client
        .send(Request::TrustMode {
            mode: new_mode.map(String::from),
        })
        .await?;
    let Response::TrustMode {
        previous,
        current,
        persisted,
    } = resp
    else {
        anyhow::bail!("unexpected response from daemon: {resp:?}");
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "source": "daemon",
                "previous": previous,
                "current": current,
                "persisted": persisted,
            })
        );
    } else if previous == current {
        println!("{current}");
    } else {
        eprintln!(
            "trust mode: {previous} → {current} ({})",
            if persisted {
                "trust file updated"
            } else {
                "in-memory only"
            }
        );
    }
    Ok(())
}

fn list(path: &Path, json: bool) -> Result<()> {
    let list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    if json {
        let entries: Vec<_> = list.iter().collect();
        let body = serde_json::json!({
            "path":  path,
            "mode":  list.mode(),
            "count": list.len(),
            "peers": entries,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    eprintln!("trust file: {}", path.display());
    eprintln!("mode:       {:?}", list.mode());
    eprintln!("entries:    {}", list.len());
    for entry in list.iter() {
        let caps = if entry.capabilities.is_empty() {
            "(any)".to_string()
        } else {
            entry.capabilities.join(", ")
        };
        let notes = entry.notes.as_deref().unwrap_or("");
        println!(
            "  {}  {:<24} caps=[{}] {}",
            &entry.pubkey[..16.min(entry.pubkey.len())],
            entry.nickname,
            caps,
            notes
        );
    }
    Ok(())
}

fn add(
    path: &Path,
    pubkey: String,
    nickname: String,
    capabilities: Vec<String>,
    notes: Option<String>,
    json: bool,
) -> Result<()> {
    let mut list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    // First add: bump mode from Off to Enforce so the trust list is actually
    // applied. Operators who want Log can `lmao trust mode log` afterwards.
    if matches!(list.mode(), TrustMode::Off) && list.is_empty() {
        list.set_mode(TrustMode::Enforce);
    }
    list.add(TrustEntry {
        pubkey: pubkey.clone(),
        nickname: nickname.clone(),
        capabilities,
        notes,
        added_at: SystemTime::now(),
    });
    list.save_to(path)
        .with_context(|| format!("saving trust file {}", path.display()))?;
    if json {
        let body = serde_json::json!({
            "added": pubkey,
            "nickname": nickname,
            "trust_file": path,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        eprintln!("added {nickname} ({pubkey}) to {}", path.display());
    }
    Ok(())
}

fn remove(path: &Path, target: &str, json: bool) -> Result<()> {
    let mut list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    // Try as pubkey first, fall back to nickname.
    let dropped = list
        .remove(target)
        .or_else(|| list.remove_by_nickname(target));
    let dropped = dropped.ok_or_else(|| anyhow!("no entry matched {target}"))?;
    list.save_to(path)
        .with_context(|| format!("saving trust file {}", path.display()))?;
    if json {
        let body = serde_json::json!({
            "removed": dropped.pubkey,
            "nickname": dropped.nickname,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        eprintln!("removed {} ({})", dropped.nickname, dropped.pubkey);
    }
    Ok(())
}

fn mode(path: &Path, new_mode: Option<&str>, json: bool) -> Result<()> {
    let mut list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    let current = list.mode();

    let Some(new) = new_mode else {
        if json {
            println!("{}", serde_json::to_string_pretty(&current)?);
        } else {
            println!("{current:?}");
        }
        return Ok(());
    };

    let next = match new.to_ascii_lowercase().as_str() {
        "off" => TrustMode::Off,
        "enforce" => TrustMode::Enforce,
        "log" => TrustMode::Log,
        other => return Err(anyhow!("unknown mode: {other} (expected off|enforce|log)")),
    };
    list.set_mode(next);
    list.save_to(path)
        .with_context(|| format!("saving trust file {}", path.display()))?;
    if json {
        let body = serde_json::json!({"previous": current, "current": next});
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        eprintln!("trust mode: {current:?} → {next:?}");
    }
    Ok(())
}

fn import(path: &Path, src: &str, json: bool) -> Result<()> {
    let mut list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;

    let text = if src == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading trust import from stdin")?;
        s
    } else {
        std::fs::read_to_string(src).with_context(|| format!("reading {src}"))?
    };
    let incoming =
        TrustList::from_toml_str(&text).with_context(|| format!("parsing {src} as TOML"))?;

    let added = list.merge(incoming);
    list.save_to(path)
        .with_context(|| format!("saving trust file {}", path.display()))?;
    if json {
        let body = serde_json::json!({"added": added, "trust_file": path});
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        eprintln!(
            "imported {added} new entries from {src} into {}",
            path.display()
        );
    }
    Ok(())
}

/// Derive the secp256k1 pubkey from a keyfile (creating it if missing)
/// without spinning up a transport. Mirrors what `LmaoNode::from_keyfile`
/// does internally but skips every other lifecycle bit.
fn pubkey(keyfile: Option<&Path>, json: bool) -> Result<()> {
    let path = keyfile.ok_or_else(|| {
        anyhow!("`lmao trust pubkey` requires --keyfile (the path to a persistent identity file)")
    })?;
    let transport = logos_messaging_a2a_transport::memory::InMemoryTransport::new();
    let node = logos_messaging_a2a_node::LmaoNode::from_keyfile(
        "trust-pubkey",
        "trust-pubkey",
        vec![],
        transport,
        path,
    )
    .with_context(|| format!("loading keyfile {}", path.display()))?;
    let pk = node.pubkey().to_string();
    if json {
        println!("{}", serde_json::json!({ "pubkey": pk, "keyfile": path }));
    } else {
        println!("{pk}");
    }
    Ok(())
}

fn export(path: &Path) -> Result<()> {
    let list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    let toml = list
        .to_toml_string()
        .context("serialising trust list to TOML")?;
    print!("{toml}");
    Ok(())
}
