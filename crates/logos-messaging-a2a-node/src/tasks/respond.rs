use logos_messaging_a2a_core::{topics, AgentCard, Task};
use logos_messaging_a2a_transport::Transport;
use std::time::Instant;

use crate::history::{now_ms, HistoryEntry};
use crate::metrics::Metrics;
use crate::{LmaoNode, Result};

impl<T: Transport> LmaoNode<T> {
    /// Respond to a task: send back a completed task with result.
    ///
    /// Publishes the response as a raw A2AEnvelope (not SDS-wrapped) to
    /// match the request side's `delegate_to_peer` flow, which uses raw
    /// `transport.publish` to avoid SDS causal-ordering deadlocks. A
    /// long-running responder's SDS bloom + local_history accumulates
    /// messages from prior peers; if we wrap the response in SDS, those
    /// causal-history entries point at messages a fresh requester has
    /// never seen, so the requester's `dependencies_satisfied` check
    /// fails and the response is buffered indefinitely. Delegation is a
    /// one-shot request/response, not a long-lived ordered stream — SDS
    /// adds no value here and breaks the common "receiver has been up
    /// for a while" case.
    pub async fn respond(&self, task: &Task, result_text: &str) -> Result<()> {
        self.respond_to(task, result_text, None).await
    }

    /// Respond to a task with `TaskState::Failed`. Used when the
    /// `--exec` returned non-zero so the sender's UI can render the
    /// task with an error state rather than treating an error message
    /// in the response body as a successful answer.
    pub async fn respond_failed(&self, task: &Task, error_text: &str) -> Result<()> {
        self.publish_response(task, error_text, None, true).await
    }

    /// Respond to a task, optionally encrypting to the sender.
    pub async fn respond_to(
        &self,
        task: &Task,
        result_text: &str,
        sender_card: Option<&AgentCard>,
    ) -> Result<()> {
        self.publish_response(task, result_text, sender_card, false).await
    }

    /// Internal — build the response task (Completed or Failed),
    /// publish it, and persist a history row tagged success/failure.
    async fn publish_response(
        &self,
        task: &Task,
        body_text: &str,
        sender_card: Option<&AgentCard>,
        failed: bool,
    ) -> Result<()> {
        let started = Instant::now();
        let response = if failed {
            task.respond_failed(body_text)
        } else {
            task.respond(body_text)
        };
        let topic = topics::task_topic(&response.to);
        let payload = self.prepare_payload(&response, sender_card).await?;

        self.channel.transport().publish(&topic, &payload).await?;
        Metrics::inc(&self.metrics.responses_sent);
        Metrics::inc(&self.metrics.messages_published);

        // Persist a "received" history row so the operator can see
        // every task this agent fielded — across daemon restarts.
        if let Some(history) = self.history.as_ref() {
            let peer_name = self
                .peer_map
                .get(&task.from)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let body = body_text.to_string();
            let cid = crate::delegation::extract_codex_cid(&body);
            let entry = HistoryEntry {
                task_id: task.id.clone(),
                parent_id: String::new(),
                created_at_ms: now_ms(),
                direction: "received".to_string(),
                peer_pubkey: task.from.clone(),
                peer_name,
                capability: String::new(),
                text: task.text().unwrap_or("").to_string(),
                body,
                cid,
                success: !failed,
                error: if failed {
                    Some(body_text.to_string())
                } else {
                    None
                },
                elapsed_ms: started.elapsed().as_millis() as u64,
                session_id: task.session_id.clone().unwrap_or_default(),
            };
            if let Err(e) = history.append(&entry).await {
                tracing::warn!(err=%e, "failed to append response to history");
            }
        }

        tracing::info!(task_id = %task.id, failed, "Responded to task");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::tasks::test_support::{fast_config, MockTransport};
    use crate::LmaoNode;
    use logos_messaging_a2a_core::{topics, Task};

    #[tokio::test]
    async fn test_respond_publishes_to_sender_topic() {
        let transport = MockTransport::new();
        let published = transport.published.clone();

        let receiver = LmaoNode::with_config(
            "receiver",
            "receiver",
            vec![],
            transport.clone(),
            fast_config(),
        );
        let rpk = receiver.pubkey().to_string();
        let _ = receiver.poll_tasks().await.unwrap();

        let sender =
            LmaoNode::with_config("sender", "sender", vec![], transport.clone(), fast_config());
        let spk = sender.pubkey().to_string();
        let _ = sender.poll_tasks().await.unwrap();

        // Sender sends task to receiver
        let task = Task::new(&spk, &rpk, "question?");
        sender.send_task(&task).await.unwrap();

        let tasks = receiver.poll_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);

        // Record message count before respond
        let pre_count = published.lock().unwrap().len();
        receiver.respond(&tasks[0], "answer!").await.unwrap();
        let post_count = published.lock().unwrap().len();
        assert!(post_count > pre_count, "respond should publish a message");

        // Verify the response was published to the SENDER's task topic
        let sender_topic = topics::task_topic(&spk);
        let pubs = published.lock().unwrap();
        let to_sender = pubs.iter().filter(|(t, _)| *t == sender_topic).count();
        assert!(to_sender >= 1, "response should target sender's topic");
    }

    #[tokio::test]
    async fn test_respond_to_encrypted_publishes() {
        let transport = MockTransport::new();
        let published = transport.published.clone();

        let receiver = LmaoNode::new_encrypted(
            "enc-receiver",
            "encrypted receiver",
            vec![],
            transport.clone(),
        );
        let rpk = receiver.pubkey().to_string();
        let _ = receiver.poll_tasks().await.unwrap();

        let sender =
            LmaoNode::new_encrypted("enc-sender", "encrypted sender", vec![], transport.clone());
        let spk = sender.pubkey().to_string();
        let _ = sender.poll_tasks().await.unwrap();

        // Send encrypted task
        let task = Task::new(&spk, &rpk, "secret question");
        sender
            .send_task_to(&task, Some(&receiver.card))
            .await
            .unwrap();

        let tasks = receiver.poll_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text(), Some("secret question"));

        // Respond with encryption — verify it publishes to sender's topic
        let pre_count = published.lock().unwrap().len();
        receiver
            .respond_to(&tasks[0], "secret answer", Some(&sender.card))
            .await
            .unwrap();
        let post_count = published.lock().unwrap().len();
        assert!(post_count > pre_count, "encrypted respond should publish");

        let sender_topic = topics::task_topic(&spk);
        let pubs = published.lock().unwrap();
        let to_sender = pubs.iter().filter(|(t, _)| *t == sender_topic).count();
        assert!(
            to_sender >= 1,
            "encrypted response should target sender's topic"
        );
    }

    #[tokio::test]
    async fn test_multiple_responses_ordering() {
        let transport = MockTransport::new();

        let server =
            LmaoNode::with_config("server", "server", vec![], transport.clone(), fast_config());
        let spk = server.pubkey().to_string();
        let _ = server.poll_tasks().await.unwrap();

        let client =
            LmaoNode::with_config("client", "client", vec![], transport.clone(), fast_config());
        let cpk = client.pubkey().to_string();
        let _ = client.poll_tasks().await.unwrap();

        for i in 0..3 {
            let task = Task::new(&cpk, &spk, &format!("task-{i}"));
            client.send_task(&task).await.unwrap();
        }

        let tasks = server.poll_tasks().await.unwrap();
        assert_eq!(tasks.len(), 3);

        for (i, task) in tasks.iter().enumerate() {
            server.respond(task, &format!("done-{i}")).await.unwrap();
        }

        let responses = client.poll_tasks().await.unwrap();
        assert_eq!(responses.len(), 3);
    }
}
