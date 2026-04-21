use oh_swarm::{InProcessBackend, TeamManager, TeamId, TeammateConfig, TeammateId};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

fn long_running_config(name: &str) -> TeammateConfig {
    let n = name.to_string();
    TeammateConfig::with_body(n, |cancel, _mb| async move {
        cancel.cancelled().await;
    })
}

#[tokio::test]
async fn create_team_add_list_remove() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(InProcessBackend::new(dir.path()));
    let mgr = TeamManager::new(dir.path().to_path_buf(), backend.clone());

    let team = TeamId::new("alpha");
    mgr.create_team(team.clone()).await.unwrap();

    let alice = TeammateId::new("alice");
    let bob = TeammateId::new("bob");

    mgr.add_member(&team, alice.clone(), long_running_config("alice"))
        .await
        .unwrap();
    mgr.add_member(&team, bob.clone(), long_running_config("bob"))
        .await
        .unwrap();

    let members = mgr.list_members(&team).await.unwrap();
    assert_eq!(members.len(), 2, "should have 2 members");
    assert!(members.contains(&alice));
    assert!(members.contains(&bob));

    mgr.remove_member(&team, &alice).await.unwrap();

    let members_after = mgr.list_members(&team).await.unwrap();
    assert_eq!(members_after.len(), 1, "should have 1 member after remove");
    assert!(members_after.contains(&bob));
    assert!(!members_after.contains(&alice));
}

#[tokio::test]
async fn team_state_persists_across_manager_instances() {
    let dir = tempfile::tempdir().unwrap();

    // First manager: create team and add a member (headless so no task is
    // stored in the backend — we just test file persistence).
    {
        let backend = Arc::new(InProcessBackend::new(dir.path()));
        let mgr = TeamManager::new(dir.path().to_path_buf(), backend.clone());
        let team = TeamId::new("beta");
        mgr.create_team(team.clone()).await.unwrap();
        mgr.add_member(
            &team,
            TeammateId::new("agent1"),
            long_running_config("agent1"),
        )
        .await
        .unwrap();
    }

    // Second manager, same root — should see the persisted member list.
    {
        let backend2 = Arc::new(InProcessBackend::new(dir.path()));
        let mgr2 = TeamManager::new(dir.path().to_path_buf(), backend2);
        let team = TeamId::new("beta");
        let members = mgr2.list_members(&team).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], TeammateId::new("agent1"));
    }
}

#[tokio::test]
async fn teammates_exchange_messages_via_mailbox() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(InProcessBackend::new(dir.path()));
    let mgr = TeamManager::new(dir.path().to_path_buf(), backend.clone());

    let team = TeamId::new("chat-team");
    mgr.create_team(team.clone()).await.unwrap();

    // sender: sends one message to "receiver" then waits for cancel.
    let team_root = dir.path().to_path_buf();
    let sender_id = TeammateId::new("sender");
    let receiver_id_clone = TeammateId::new("receiver");

    let sender_cfg = {
        let tr = team_root.clone();
        let rid = receiver_id_clone.clone();
        TeammateConfig::with_body("sender", move |cancel, _mb| {
            let mb_recv = oh_swarm::Mailbox::for_agent(&tr, &rid);
            async move {
                let msg = oh_swarm::types::Message::new(
                    oh_swarm::types::TeammateId::new("sender"),
                    oh_swarm::types::TeammateId::new("receiver"),
                    oh_swarm::types::MessageKind::AgentReply,
                    serde_json::json!({ "greeting": "hi from sender" }),
                );
                mb_recv.send(&msg).await.expect("send failed");
                cancel.cancelled().await;
            }
        })
    };

    // receiver: waits until a message is available, stores it, then exits.
    let received_flag = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let received_flag2 = received_flag.clone();

    let receiver_cfg = {
        TeammateConfig::with_body("receiver", move |cancel, mb| {
            let flag = received_flag2.clone();
            async move {
                // Poll up to 500ms for the message.
                for _ in 0..50 {
                    if let Ok(Some(m)) = mb.recv_one().await {
                        let greeting = m.body["greeting"].as_str().unwrap_or("").to_string();
                        *flag.lock().await = Some(greeting);
                        return;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
                cancel.cancelled().await;
            }
        })
    };

    mgr.add_member(&team, sender_id.clone(), sender_cfg).await.unwrap();
    mgr.add_member(&team, TeammateId::new("receiver"), receiver_cfg).await.unwrap();

    // Allow tasks time to run.
    sleep(Duration::from_millis(300)).await;

    let guard = received_flag.lock().await;
    assert_eq!(
        guard.as_deref(),
        Some("hi from sender"),
        "receiver did not get the message in time"
    );
}
