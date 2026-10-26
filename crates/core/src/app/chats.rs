//! Low-priority chat work on the shared embedding channel.

use std::time::{Duration, Instant};

use crate::db::chats;
use crate::provider::embedding::Embedder;

use super::App;

pub(super) struct ChatEmbedJob {
    identity: String,
    chunks: Vec<(String, i64, String)>,
}

impl App {
    pub(super) fn index_chats(&mut self) {
        let _ = chats::index_pending(self.db.conn());
        self.update_chat_index_status();
        self.next_chat_index = Instant::now() + Duration::from_secs(5);
    }

    pub(super) fn update_chat_index_status(&mut self) {
        let identity = self
            .embedding_backend
            .as_ref()
            .and_then(|b| b.identity())
            .map(crate::provider::embedding::EmbedIdentity::key);
        if let Ok((indexed, total)) =
            chats::progress(self.db.conn(), &self.active_space.id, identity.as_deref())
        {
            self.chat_index_status = format!("chats: {indexed} of {total} indexed");
        }
    }

    pub(super) fn start_chat_embedding(&mut self, embedder: Embedder, identity: String) {
        let Ok(chunks) = chats::missing_vectors(self.db.conn(), &identity) else {
            return;
        };
        if chunks.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let inputs = chunks
            .iter()
            .map(|(_, _, text)| embedder.document(chats::CHUNK_TITLE, text))
            .collect();
        self.embedding_chat_job = Some(ChatEmbedJob { identity, chunks });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.embed_rx = Some(rx);
        let space = self.active_space.id.clone();
        handle.spawn(async move {
            let result = embedder
                .embed(inputs)
                .await
                .map(|vectors| {
                    vectors
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (i64::try_from(i).expect("bounded embedding batch"), v))
                        .collect()
                })
                .map_err(|e| e.to_string());
            let _ = tx.send((space, "chats".to_string(), result));
        });
    }

    pub(super) fn on_chat_embed_done(&mut self, result: Result<Vec<(i64, Vec<f32>)>, String>) {
        let Some(job) = self.embedding_chat_job.take() else {
            return;
        };
        let result = result.and_then(|vectors| {
            if vectors.len() != job.chunks.len() {
                return Err("embedding batch size mismatch".to_string());
            }
            chats::store_vectors(self.db.conn(), &job.identity, &job.chunks, &vectors)
                .map_err(|e| e.to_string())
        });
        if let Err(error) = result {
            self.embedding_status = format!("embeddings: {error} · keyword search available");
            self.next_chat_index = Instant::now() + Duration::from_secs(60);
        }
        self.update_chat_index_status();
        // Do not chain chat jobs: one bounded batch per background tick leaves
        // foreground work and newly imported files priority over backfill.
    }
}

#[cfg(test)]
#[path = "chat_index_tests.rs"]
mod tests;
