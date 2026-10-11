use std::sync::Arc;

use peri_acp_types::session_resources::AccessMode;

use super::super::mutation::StoreAccess;
use super::super::recovery_fixture_tests::Harness;
use super::*;

#[tokio::test]
async fn test_remote_composition_passes_opened_data_and_access_to_execution_factory() {
    let harness = Harness::open(StoreAccess::ReadOnly).await;
    let expected = Arc::clone(&harness.adapter);
    let facade = assemble_remote(harness.adapter, AccessMode::ReadOnly, |data, access| {
        assert!(Arc::ptr_eq(&data, &expected));
        assert_eq!(access, AccessMode::ReadOnly);
        Ok(Arc::new(RemoteExecution::new(data, true)))
    })
    .unwrap();

    facade.close().await.unwrap();
}
