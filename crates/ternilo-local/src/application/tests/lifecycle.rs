use super::*;

#[tokio::test]
async fn data_directory_allows_only_one_application_writer() {
    let data_dir = test_data_dir();
    let first = open_test_application(data_dir.clone()).await;
    let second = LocalApplication::open(
        crate::catalog().unwrap(),
        crate::local_profile(),
        HostPolicy::local(RunLimits::default()),
        data_dir.clone(),
    )
    .await;
    let Err(error) = second else {
        panic!("a second application opened the same data directory");
    };
    assert!(error.message.contains("already open"), "{error}");

    first.shutdown().await.unwrap();
    drop(first);
    let reopened = open_test_application(data_dir.clone()).await;
    reopened.shutdown().await.unwrap();
    drop(reopened);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
}
