use std::path::Path;

use codex_micro_chroma::service::render_launch_agent;

#[test]
fn launch_agent_runs_the_local_worker_and_preserves_spaces() {
    let plist = render_launch_agent(
        Path::new("/Users/example/Application Support/codex-micro-chroma"),
        Path::new("/Users/example/Logs/Codex & Chroma"),
    );

    assert!(
        plist.contains("<string>/Users/example/Application Support/codex-micro-chroma</string>")
    );
    assert!(plist.contains("<string>run</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    assert!(plist.contains("<key>LimitLoadToSessionType</key>\n    <string>Aqua</string>"));
    assert!(plist.contains("Codex &amp; Chroma"));
    assert!(!plist.contains("OpenAI"));
}
