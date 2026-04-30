//! `lmao trust` subcommand handlers.
//!
//! All actions read and write the same TOML file the daemon loads on
//! startup (`--trust-file`, default `$XDG_CONFIG_HOME/lmao/trust.toml`).

use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_core::{TrustEntry, TrustList, TrustMode};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cli::TrustAction;

pub async fn handle(action: TrustAction, trust_file: Option<PathBuf>, json: bool) -> Result<()> {
    let path = trust_file.unwrap_or_else(TrustList::default_path);
    match action {
        TrustAction::List => list(&path, json),
        TrustAction::Add {
            pubkey,
            nickname,
            capabilities,
            notes,
        } => add(&path, pubkey, nickname, capabilities, notes, json),
        TrustAction::Remove { target } => remove(&path, &target, json),
        TrustAction::Mode { new_mode } => mode(&path, new_mode.as_deref(), json),
        TrustAction::Import { path: src } => import(&path, &src, json),
        TrustAction::Export => export(&path),
    }
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

fn export(path: &Path) -> Result<()> {
    let list = TrustList::load_from(path)
        .with_context(|| format!("loading trust file {}", path.display()))?;
    let toml = list
        .to_toml_string()
        .context("serialising trust list to TOML")?;
    print!("{toml}");
    Ok(())
}
