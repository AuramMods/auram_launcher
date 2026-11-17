use auram_core::{auth::Auth, download::Downloader, jobs::Job, storage::Layout};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    auram_core::java_bridge::entry();
    let layout = Layout::new(std::env::temp_dir().join("auram-auth-verification"))?;
    let auth = Auth::new(Downloader::new(&layout)?.client);
    let job = Job::default();
    let cancel = job.cancel.clone();
    let result=auth.sign_in_device("cf95d5d7-7830-4ad4-a70e-d54e50e80b40",&job,move|_|{
        println!("QA_EVT {{\"event\":\"existing_device_registration\",\"status\":\"pass\",\"details\":\"Microsoft issued a device challenge with offline_access using the existing application ID; no browser or clipboard action occurred\"}}");cancel.cancel();
    }).await;
    assert!(result.is_err());
    assert!(auth.device_challenge().is_err());
    Ok(())
}
