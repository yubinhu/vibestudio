//! Progressive discovery returns global/known skills before the home crawl and
//! revalidates those folders on every read. Exercise the public HTTP contract in
//! a private home; `dirs::home_dir()` honors HOME on Unix.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use skill_server::{spawn, ServerConfig};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_skill(root: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n# Skill\n"),
    )
    .unwrap();
}

fn get(client: &ureq::Agent, base: &str, query: &str) -> Value {
    let body = client
        .get(&format!("{base}/api/skills/discover{query}"))
        .call()
        .expect("discovery response")
        .into_string()
        .expect("discovery body");
    serde_json::from_str(&body).expect("discovery JSON")
}

fn skill<'a>(groups: &'a Value, root: &Path) -> Option<&'a Value> {
    let root = root.to_string_lossy();
    groups
        .as_array()
        .expect("agent groups")
        .iter()
        .flat_map(|group| group["skills"].as_array().expect("group skills"))
        .find(|skill| skill["root"].as_str() == Some(root.as_ref()))
}

fn completed(client: &ureq::Agent, base: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let response = get(client, base, "?progressive=true");
        if !response["scanning"].as_bool().expect("scanning flag") {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "background discovery did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn discovery_returns_fast_inventory_then_updates_projects_and_revalidates_known_folders() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "vibestudio-discovery-{}-{nonce}",
        std::process::id()
    )));
    let home = fixture.0.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(&config).unwrap();

    // This binary contains only this test. Set the environment before starting
    // any server threads, and leave it isolated for their remaining lifetime.
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", &config);

    // Official global skills and project skills are never auto-git-tracked.
    let global = home.join(".codex/skills/.system/global");
    let project = home.join("work/repo/.agents/skills/project");
    write_skill(&global, "global", "Global fixture");
    write_skill(&project, "project", "Project fixture");

    let server = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .expect("isolated discovery server");
    let base = server.url();
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();

    // Legacy clients cannot poll for background results. Their very first
    // request must include project skills without priming the location index.
    let legacy = get(&client, &base, "");
    assert!(legacy.is_array(), "older clients retain the array contract");
    assert!(skill(&legacy, &global).is_some());
    assert!(skill(&legacy, &project).is_some());

    // Assert the phase boundary, not a machine-dependent latency threshold.
    let first = get(&client, &base, "?progressive=true");
    assert_eq!(first["scanning"], true);
    let found_global = skill(&first["groups"], &global).expect("global on first response");
    assert_eq!(found_global["name"], "global");
    assert_eq!(found_global["kind"], "official");
    assert!(skill(&first["groups"], &project).is_none());

    let full = completed(&client, &base);
    let found_project = skill(&full["groups"], &project).expect("background project discovery");
    assert_eq!(found_project["name"], "project");
    assert_eq!(found_project["project"], "repo");
    assert_eq!(found_project["kind"], "personal");

    // Metadata and additions/deletions inside known roots remain live even
    // during the cooldown; the cache stores locations, not skill records.
    write_skill(&project, "project-renamed", "Updated description");
    let sibling = project.parent().unwrap().join("sibling");
    write_skill(&sibling, "sibling", "New skill in a known root");
    let revalidated = get(&client, &base, "?progressive=true");
    assert_eq!(revalidated["scanning"], false);
    let updated = skill(&revalidated["groups"], &project).expect("revalidated project");
    assert_eq!(updated["name"], "project-renamed");
    assert_eq!(updated["description"], "Updated description");
    assert!(skill(&revalidated["groups"], &sibling).is_some());

    std::fs::remove_file(project.join("SKILL.md")).unwrap();
    let deleted = get(&client, &base, "?progressive=true");
    assert_eq!(deleted["scanning"], false);
    assert!(skill(&deleted["groups"], &project).is_none());
    assert!(skill(&deleted["groups"], &sibling).is_some());

    // Ordinary polls don't crawl the home repeatedly. An explicit refresh
    // bypasses the cooldown and finds projects outside the remembered roots.
    let new_project = home.join("other/new-repo/.agents/skills/new-project");
    write_skill(&new_project, "new-project", "Newly created project");
    let ordinary = get(&client, &base, "?progressive=true");
    assert_eq!(ordinary["scanning"], false);
    assert!(skill(&ordinary["groups"], &new_project).is_none());

    let refreshing = get(&client, &base, "?progressive=true&refresh=true");
    assert_eq!(refreshing["scanning"], true);
    assert!(skill(&refreshing["groups"], &global).is_some());
    assert!(skill(&refreshing["groups"], &sibling).is_some());

    let refreshed = completed(&client, &base);
    let found_new = skill(&refreshed["groups"], &new_project).expect("explicit refresh project");
    assert_eq!(found_new["name"], "new-project");
    assert_eq!(found_new["project"], "new-repo");
    assert!(skill(&refreshed["groups"], &project).is_none());
    assert!(!global.join(".git").exists());
    assert!(!sibling.join(".git").exists());
}
