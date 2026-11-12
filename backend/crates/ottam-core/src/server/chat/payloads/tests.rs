use super::*;
use serde_json::json;

#[test]
fn payload_normalization_distinguishes_omission_empty_and_terminal_deltas() {
    let mut omitted = json!({"status":"completed"});
    assert!(extract(&mut omitted)[0].parts.is_none());
    let mut empty = json!({"rawOutput":""});
    assert!(extract(&mut empty)[0].parts.as_ref().unwrap()[0]
        .bytes
        .is_empty());
    let mut terminal = json!({"_meta":{"terminal_output":{"data":"delta"},"terminal_exit":{"exit_code":9}},"content":[{"type":"terminal","terminalId":"t"}]});
    let delta = extract(&mut terminal);
    assert!(delta[0].append);
    assert_eq!(terminal["exitCode"], 9);
    let mut terminal_only = json!({"content":[{"type":"terminal","terminalId":"t"}]});
    assert!(extract(&mut terminal_only)[0].parts.is_none());
    let mut patch =
        json!({"name":"apply_patch","rawInput":{"patch":"*** Begin Patch\n*** End Patch"}});
    let input = extract(&mut patch);
    assert_eq!(input[0].role, "fileChangeInput");
    assert_eq!(input[0].parts.as_ref().unwrap()[0].name, "Patch");
    assert!(patch["rawInput"]["patch"].is_null());
}

/// An MCP call keeps its arguments even when its caption or arguments look
/// like a file write (omp captions its `xd://` calls; `normalize_tool` agrees).
#[test]
fn an_mcp_call_is_not_captured_as_a_file_change() {
    let mut call = json!({"name":"mcp__notes_write_note", "kind":"other", "title":"Edit the note",
        "rawInput":{"path":"notes.md", "content":"hello", "text":"hi"}});
    let updates = extract(&mut call);
    assert!(updates.iter().all(|update| update.role != "fileChangeInput"));
    assert_eq!(
        call["rawInput"],
        json!({"path":"notes.md", "content":"hello", "text":"hi"})
    );

    // Not every omp device is an MCP tool; the stamp marks a device run.
    let mut device = json!({"name":"report_issue", "kind":"other", "title":"Write up the crash",
        "ottamDeviceCall": true, "rawInput":{"text":"crash on save"}});
    assert!(extract(&mut device)
        .iter()
        .all(|update| update.role != "fileChangeInput"));
    assert_eq!(device["rawInput"], json!({"text":"crash on save"}));
}

#[test]
fn payload_supplied_diffs_and_wrapped_patches_are_captured_once() {
    let mut diff = json!({"kind":"edit","content":[{"type":"diff","path":"a","oldText":"old","newText":"new"}]});
    let updates = extract(&mut diff);
    assert_eq!(updates[0].role, "fileChangeInput");
    assert_eq!(updates[0].parts.as_ref().unwrap().len(), 2);
    assert!(updates[1].parts.is_none());
    let mut duplicate = json!({"kind":"edit","rawInput":{"old_string":"old","new_string":"new"},"content":[{"type":"diff","path":"a","oldText":"old","newText":"new"}]});
    let updates = extract(&mut duplicate);
    assert_eq!(
        updates
            .iter()
            .filter_map(|u| u.parts.as_ref())
            .map(Vec::len)
            .sum::<usize>(),
        2
    );
    let mut patch = json!({"title":"Apply patch","rawInput":{"name":"apply_patch","arguments":"*** Begin Patch"}});
    let updates = extract(&mut patch);
    assert_eq!(updates[0].parts.as_ref().unwrap()[0].name, "Patch");
    assert!(patch["rawInput"]["arguments"].is_null());
}
