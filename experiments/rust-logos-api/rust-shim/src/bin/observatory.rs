// lmao-observatory — terminal UI for live-watching an agent fleet.
//
// What you see:
//   - Top bar:    your agent's identity (name, pubkey, capabilities,
//                 load, uptime).
//   - Tab Peers:  live PeerMap — name, capabilities, load, last-seen
//                 age. One row per peer, ordered by recency.
//   - Tab Tasks:  recent task history — direction (sent/received),
//                 capability, peer, status, latency, body preview, CID.
//   - Tab Trust:  trust list — mode + entries.
//   - Footer:     last refresh time + key hints.
//
// How it works:
//   - One `Shim` instance owns the Qt thread; we hold it inside an
//     `Arc<Mutex<Shim>>` shared across the polling tasks.
//   - Tokio task per metric, each with its own interval. `shim.call`
//     is blocking C++ on the Qt thread, so we wrap each call in
//     `spawn_blocking` to keep tokio's executor free.
//   - Shared state behind an `Arc<RwLock<State>>`; UI thread reads,
//     pollers write.
//   - Render loop ticks at 10 Hz.
//
// Connects to whatever logos-core / logoscore is reachable via the
// usual `LOGOS_INSTANCE_ID` env handshake. If nothing is up, you'll
// see "—" everywhere and the footer will surface the connect timeout.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use logos_shim::Shim;

const REFRESH_FAST_SECS: u64 = 2;   // info, peers — they change quickly
const REFRESH_SLOW_SECS: u64 = 10;  // task history, trust — slower-changing

// ── State ───────────────────────────────────────────────────────────

#[derive(Default)]
struct State {
    info: Option<Value>,
    peers: Vec<Value>,
    history: Vec<Value>,
    trust: Option<Value>,
    last_refresh: Option<Instant>,
    last_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Peers,
    Tasks,
    Trust,
}

impl Tab {
    fn next(self) -> Self {
        match self {
            Tab::Peers => Tab::Tasks,
            Tab::Tasks => Tab::Trust,
            Tab::Trust => Tab::Peers,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Tab::Peers => "Peers",
            Tab::Tasks => "Tasks",
            Tab::Trust => "Trust",
        }
    }
}

// ── Polling ─────────────────────────────────────────────────────────

async fn poll_once(
    shim: &Arc<Mutex<Shim>>,
    target: &str,
    method: &str,
    args: &str,
    timeout_ms: i32,
) -> Result<Value> {
    let shim = shim.clone();
    let target = target.to_string();
    let method = method.to_string();
    let args = args.to_string();
    let json = tokio::task::spawn_blocking(move || {
        let g = shim.blocking_lock();
        g.call(&target, &method, &args, timeout_ms)
    })
    .await??;
    Ok(serde_json::from_str(&json).unwrap_or(Value::String(json)))
}

async fn poller(shim: Arc<Mutex<Shim>>, state: Arc<RwLock<State>>) {
    let mut fast = tokio::time::interval(Duration::from_secs(REFRESH_FAST_SECS));
    let mut slow = tokio::time::interval(Duration::from_secs(REFRESH_SLOW_SECS));
    loop {
        tokio::select! {
            _ = fast.tick() => {
                let info = poll_once(&shim, "agent", "info", "[]", 5_000).await.ok();
                let peers_resp = poll_once(&shim, "agent", "peers", "[\"\"]", 5_000).await.ok();
                let mut s = state.write().await;
                if let Some(v) = info {
                    s.info = Some(v);
                }
                if let Some(v) = peers_resp {
                    // Daemon shape: { "kind":"presence_peers", "peers":[…] }
                    s.peers = v.get("peers").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                }
                s.last_refresh = Some(Instant::now());
            }
            _ = slow.tick() => {
                let history_resp = poll_once(
                    &shim, "agent", "task_history_list",
                    "[50, 0, \"\", \"\"]", 5_000,
                ).await.ok();
                let trust = poll_once(&shim, "agent", "trust_list", "[]", 5_000).await.ok();
                let mut s = state.write().await;
                if let Some(v) = history_resp {
                    s.history = v.get("entries").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                }
                if let Some(v) = trust {
                    s.trust = Some(v);
                }
            }
        }
    }
}

// ── Rendering helpers ───────────────────────────────────────────────

fn short_pubkey(pk: &str) -> String {
    if pk.len() > 12 {
        format!("{}…", &pk[..12])
    } else {
        pk.to_string()
    }
}

fn ago(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else {
        format!("{}h ago", seconds / 3600)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Drawing ─────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect, info: Option<&Value>) {
    let line = if let Some(info) = info {
        let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("—");
        let pk = info.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        let caps = info
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let bucket = info
            .get("load")
            .and_then(|l| l.get("bucket"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let queue = info
            .get("load")
            .and_then(|l| l.get("queue_depth"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max = info
            .get("load")
            .and_then(|l| l.get("max_concurrent"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let uptime = info.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
        let storage_ok = info
            .get("storage_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let err = info.get("error").and_then(|v| v.as_str());
        if let Some(msg) = err {
            Line::from(vec![
                Span::styled(
                    "agent: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(msg.to_string(), Style::default().fg(Color::Red)),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!("{name} "), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(short_pubkey(pk), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled("caps=", Style::default().fg(Color::DarkGray)),
                Span::raw(if caps.is_empty() { "—".into() } else { caps }),
                Span::raw("  "),
                Span::styled("load=", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{bucket} {queue}/{max}"),
                    match bucket {
                        "free" => Style::default().fg(Color::Green),
                        "busy" => Style::default().fg(Color::Yellow),
                        "full" => Style::default().fg(Color::Red),
                        _ => Style::default(),
                    },
                ),
                Span::raw("  "),
                Span::styled("storage=", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if storage_ok { "ok" } else { "off" },
                    if storage_ok {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Yellow)
                    },
                ),
                Span::raw("  "),
                Span::styled("uptime=", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}s", uptime)),
            ])
        }
    } else {
        Line::from(Span::styled(
            "connecting…",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ))
    };
    f.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("identity")),
        area,
    );
}

fn draw_peers(f: &mut Frame, area: Rect, peers: &[Value]) {
    let now = now_secs();
    let header = Row::new(vec!["NAME", "PUBKEY", "CAPABILITIES", "LOAD", "LAST SEEN"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = peers
        .iter()
        .map(|p| {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let pk = p.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
            let caps = p
                .get("capabilities")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let bucket = p
                .get("load")
                .and_then(|l| l.get("bucket"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let last_seen = p
                .get("last_seen_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let load_style = match bucket {
                "free" => Style::default().fg(Color::Green),
                "busy" => Style::default().fg(Color::Yellow),
                "full" => Style::default().fg(Color::Red),
                _ => Style::default().fg(Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(name.to_string()),
                Cell::from(short_pubkey(pk)).style(Style::default().fg(Color::DarkGray)),
                Cell::from(caps),
                Cell::from(bucket.to_string()).style(load_style),
                Cell::from(ago(now.saturating_sub(last_seen))),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        &[
            Constraint::Length(20),
            Constraint::Length(14),
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("peers ({})", peers.len())),
    );
    f.render_widget(table, area);
}

fn draw_tasks(f: &mut Frame, area: Rect, history: &[Value]) {
    let header = Row::new(vec!["DIR", "WHEN", "PEER", "CAP", "STATUS", "ms", "TEXT"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = history
        .iter()
        .map(|h| {
            let dir = h.get("direction").and_then(|v| v.as_str()).unwrap_or("?");
            let started_at = h
                .get("started_at_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let peer = h
                .get("peer_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| h.get("peer_pubkey").and_then(|v| v.as_str()))
                .unwrap_or("?");
            let cap = h.get("capability").and_then(|v| v.as_str()).unwrap_or("");
            let success = h.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let elapsed = h.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let text = h
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .replace('\n', " ");
            let now = now_secs();
            Row::new(vec![
                Cell::from(if dir == "sent" { "→".to_string() } else { "←".to_string() }),
                Cell::from(ago(now.saturating_sub(started_at))),
                Cell::from(short_pubkey(peer)),
                Cell::from(cap.to_string()),
                Cell::from(if success { "ok".to_string() } else { "fail".to_string() })
                    .style(if success {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Red)
                    }),
                Cell::from(format!("{elapsed}")),
                Cell::from(if text.len() > 60 {
                    format!("{}…", &text[..60])
                } else {
                    text
                }),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        &[
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Length(15),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("task history ({})", history.len())),
    );
    f.render_widget(table, area);
}

fn draw_trust(f: &mut Frame, area: Rect, trust: Option<&Value>) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(trust) = trust {
        let mode = trust.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        let entries = trust
            .get("entries")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled("mode = ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                mode.to_string(),
                match mode {
                    "off" => Style::default().fg(Color::DarkGray),
                    "log" => Style::default().fg(Color::Yellow),
                    "enforce" => Style::default().fg(Color::Green),
                    _ => Style::default(),
                },
            ),
            Span::styled(
                format!("   ({} entries)", entries.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::raw(""));
        for e in entries {
            let nick = e.get("nickname").and_then(|v| v.as_str()).unwrap_or("?");
            let pk = e.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
            let caps = e
                .get("capabilities")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(format!("  {nick:<24}"), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(short_pubkey(pk), Style::default().fg(Color::DarkGray)),
                Span::raw("  caps=["),
                Span::raw(if caps.is_empty() { "any".into() } else { caps }),
                Span::raw("]"),
            ]));
        }
    } else {
        lines.push(Line::raw("(no trust data yet)"));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("trust list")),
        area,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, state: &State) {
    let refresh = match state.last_refresh {
        Some(t) => format!("{}s ago", t.elapsed().as_secs()),
        None => "never".into(),
    };
    let line = Line::from(vec![
        Span::styled("[1/2/3]", Style::default().fg(Color::Cyan)),
        Span::raw(" tabs   "),
        Span::styled("[Tab]", Style::default().fg(Color::Cyan)),
        Span::raw(" cycle   "),
        Span::styled("[q]", Style::default().fg(Color::Cyan)),
        Span::raw(" quit   "),
        Span::styled("refresh:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {refresh}")),
        if let Some(err) = &state.last_error {
            Span::styled(format!("  err: {err}"), Style::default().fg(Color::Red))
        } else {
            Span::raw("")
        },
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn ui(f: &mut Frame, state: &State, tab: Tab) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Length(3),  // tab bar
            Constraint::Min(1),     // content
            Constraint::Length(1),  // footer
        ])
        .split(f.area());

    draw_header(f, chunks[0], state.info.as_ref());

    let titles: Vec<&str> = [Tab::Peers, Tab::Tasks, Tab::Trust]
        .iter()
        .map(|t| t.label())
        .collect();
    let selected = match tab {
        Tab::Peers => 0,
        Tab::Tasks => 1,
        Tab::Trust => 2,
    };
    f.render_widget(
        Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL))
            .select(selected)
            .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        chunks[1],
    );

    match tab {
        Tab::Peers => draw_peers(f, chunks[2], &state.peers),
        Tab::Tasks => draw_tasks(f, chunks[2], &state.history),
        Tab::Trust => draw_trust(f, chunks[2], state.trust.as_ref()),
    }

    draw_footer(f, chunks[3], state);
}

// ── Terminal lifecycle ─────────────────────────────────────────────

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(mut t: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen)?;
    t.show_cursor()?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let shim = Arc::new(Mutex::new(Shim::new("lmao-observatory")?));
    let state = Arc::new(RwLock::new(State::default()));

    tokio::spawn(poller(shim.clone(), state.clone()));

    let mut terminal = setup_terminal()?;
    let mut tab = Tab::Peers;

    let result = run_ui(&mut terminal, &state, &mut tab).await;
    restore_terminal(terminal)?;
    result
}

async fn run_ui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &Arc<RwLock<State>>,
    tab: &mut Tab,
) -> Result<()> {
    loop {
        // Snapshot the state before drawing so we don't hold the lock
        // across `terminal.draw` (which can be slow).
        let snapshot = {
            let g = state.read().await;
            // Lightweight clone of the bits the UI needs. peers/history
            // are Vec<Value> and may be a few KB each — fine.
            State {
                info: g.info.clone(),
                peers: g.peers.clone(),
                history: g.history.clone(),
                trust: g.trust.clone(),
                last_refresh: g.last_refresh,
                last_error: g.last_error.clone(),
            }
        };
        terminal.draw(|f| ui(f, &snapshot, *tab))?;

        // Poll keyboard events with a 100 ms timeout — same as our
        // render cadence.
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Tab => *tab = tab.next(),
                    KeyCode::Char('1') => *tab = Tab::Peers,
                    KeyCode::Char('2') => *tab = Tab::Tasks,
                    KeyCode::Char('3') => *tab = Tab::Trust,
                    _ => {}
                }
            }
        }
    }
}
