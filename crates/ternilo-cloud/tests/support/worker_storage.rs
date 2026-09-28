use ternilo_cloud::CloudStore;

pub async fn bind_workers(store: &CloudStore, worker_ids: &[&str], storage_id: &str) {
    sqlx::query(
        "INSERT INTO cloud_storage_roots(storage_id,root_id,registered_at_ms)
         VALUES($1,$2,0) ON CONFLICT(storage_id) DO NOTHING",
    )
    .bind(storage_id)
    .bind(format!("storage-root-{storage_id}"))
    .execute(store.database().pool())
    .await
    .unwrap();
    for worker_id in worker_ids {
        sqlx::query(
            "INSERT INTO cloud_worker_credentials(worker_id,token_hash,storage_id,created_at_ms)
             VALUES($1,$2,$3,0) ON CONFLICT(worker_id) DO NOTHING",
        )
        .bind(worker_id)
        .bind(format!("unused-storage-contract-token-{worker_id}"))
        .bind(storage_id)
        .execute(store.database().pool())
        .await
        .unwrap();
        // These database contracts use explicit logical times, without a live heartbeat task.
        let hello = ternilo_transport::ExecutorHello {
            protocol_version: ternilo_transport::EXECUTOR_PROTOCOL_VERSION,
            executor_id: ternilo_transport::ExecutorId::new(*worker_id),
            executor_kind: ternilo_transport::ExecutorKind::CloudWorker,
            instance_nonce: format!("storage-contract-{worker_id}"),
            catalog_revision: "storage-contract".to_owned(),
            capabilities: std::collections::BTreeSet::from([
                ternilo_transport::ExecutorCapability::CloudRun,
            ]),
        };
        sqlx::query("INSERT INTO cloud_workers(worker_id,instance_nonce,generation,hello_json,registered_at_ms,last_seen_at_ms,lease_expires_at_ms) VALUES($1,$2,1,$3,0,0,9223372036854775807) ON CONFLICT(worker_id) DO NOTHING")
            .bind(worker_id).bind(&hello.instance_nonce).bind(ternilo_storage::Json(&hello))
            .execute(store.database().pool()).await.unwrap();
    }
}
