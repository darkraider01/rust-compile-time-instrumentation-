pub async fn async_direct_dependency() -> u32 {
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    21
}

pub async fn async_spawn_dependency() -> u32 {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        async_spawn_child().await
    })
    .await
    .expect("demo Tokio task should complete")
}

async fn async_spawn_child() -> u32 {
    tokio::task::yield_now().await;
    21
}
