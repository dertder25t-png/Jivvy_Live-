//! `jivvy-ctl`: one-line commands to a running daemon, e.g. `jivvy-ctl next`,
//! `jivvy-ctl black off`, `jivvy-ctl state`. See `jivvy_daemon::ctl::USAGE`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use jivvy_daemon::ctl;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inv = ctl::parse(&args).unwrap_or_else(|msg| {
        eprintln!("{msg}");
        std::process::exit(2);
    });
    if let Err(e) = run(&inv) {
        eprintln!("jivvy-ctl: {e}");
        std::process::exit(1);
    }
}

fn run(inv: &ctl::Invocation) -> Result<(), String> {
    let unreachable = |e: &dyn std::fmt::Display| format!("can't reach the daemon at {}: {e}", inv.addr);
    // A name can resolve to several addresses (localhost: IPv6, then IPv4); try each in turn.
    let mut last_error = String::from("no address");
    let mut conn = None;
    for addr in inv.addr.to_socket_addrs().map_err(|e| unreachable(&e))? {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    let mut conn = conn.ok_or_else(|| unreachable(&last_error))?;
    let id = format!("ctl-{}", std::process::id());
    let line = ctl::envelope(&inv.command, &id, jivvy_daemon::now_ms());
    conn.write_all(line.as_bytes()).map_err(|e| unreachable(&e))?;
    // Timeouts go on the stream being read: on Windows a clone's timeout doesn't apply to it.
    let mut reader = BufReader::new(conn);
    let mut next_line = |timeout: Option<Duration>| -> Result<Option<String>, String> {
        reader.get_ref().set_read_timeout(timeout).map_err(|e| e.to_string())?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line.trim_end().to_string())),
            Err(e) => Err(e.to_string()),
        }
    };
    // The ack comes at once.
    let ack = match next_line(Some(Duration::from_secs(5))) {
        Ok(Some(l)) => l,
        Err(e) => return Err(format!("no answer from the engine: {e}")),
        Ok(None) => return Err("the engine closed the connection without answering".into()),
    };
    println!("{ack}");
    let ok = serde_json::from_str::<serde_json::Value>(&ack).is_ok_and(|v| v["ok"] == true);
    if !ok {
        return Err("the engine refused the command".into());
    }
    if let Some(name) = inv.until {
        // The result follows the ack; the test takes about 5 s.
        loop {
            match next_line(Some(Duration::from_secs(40))) {
                Ok(Some(event)) => {
                    if serde_json::from_str::<serde_json::Value>(&event).is_ok_and(|v| v["event"] == name) {
                        println!("{event}");
                        return Ok(());
                    }
                }
                Ok(None) => return Err("the engine closed the connection".into()),
                Err(e) => return Err(format!("no {name} result: {e}")),
            }
        }
    }
    if inv.watch {
        // A watch waits as long as it takes for the next event.
        loop {
            match next_line(None) {
                Ok(Some(event)) => println!("{event}"),
                Ok(None) => return Err("the engine closed the connection".into()),
                Err(e) => return Err(format!("connection lost: {e}")),
            }
        }
    }
    Ok(())
}
