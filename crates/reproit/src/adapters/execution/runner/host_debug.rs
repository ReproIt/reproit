use super::cell::{random_id, write_debugger_files};
use super::model::{CommandProvider, CommandResult, ProviderRun, ProviderVerdict};
use super::{catalog, process};
use anyhow::{Context, Result};
use reproit_protocol::{
    CellReceipt, CleanupStatus, DebugEndpoint, DiagnosticReceipt, ReproductionPlan, SourceMapping,
    CELL_RECEIPT_VERSION, DIAGNOSTIC_RECEIPT_VERSION,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle;

pub(super) struct HostDebugSession {
    root: PathBuf,
    provider_id: String,
    provider: CommandProvider,
    run_id: String,
    receipt_id: String,
    configuration_sha256: String,
    task: Option<JoinHandle<Result<CommandResult>>>,
}

impl HostDebugSession {
    pub(super) fn prepare(
        root: &Path,
        plan: &ReproductionPlan,
        providers: &[(String, &CommandProvider)],
    ) -> Result<Self> {
        let candidates = providers
            .iter()
            .filter(|(_, provider)| provider.debug.is_some())
            .collect::<Vec<_>>();
        let [(provider_id, provider)] = candidates.as_slice() else {
            anyhow::bail!(
                "diagnostic execution requires exactly one bound provider debug capability"
            );
        };
        let root = root.canonicalize().context("resolving checkout root")?;
        let run_id = random_id("run")?;
        let receipt_id = random_id("cell")?;
        let directory = root.join(".reproit").join("cells").join(&run_id);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let provider = (*provider).clone();
        let provider_digest = catalog::provider_digest(&provider)?;
        let configuration_sha256 = provider_digest
            .strip_prefix("sha256:")
            .context("provider digest lost its sha256 prefix")?
            .to_string();
        if provider.phase != crate::domain::execution::ExecutionPhase::Trigger {
            anyhow::bail!("host debug provider must own the trigger phase");
        }
        if !plan
            .bindings
            .iter()
            .any(|binding| binding.provider_id == **provider_id)
        {
            anyhow::bail!("host debug provider is not bound to the replay plan");
        }
        Ok(Self {
            root,
            provider_id: (*provider_id).clone(),
            provider,
            run_id,
            receipt_id,
            configuration_sha256,
            task: None,
        })
    }

    pub(super) async fn start(&mut self, occurrence_id: &str) -> Result<DiagnosticReceipt> {
        let profile = self
            .provider
            .debug
            .clone()
            .context("host debug profile disappeared")?;
        let root = self.root.clone();
        let argv = profile.argv.clone();
        let environment = self.provider.environment.clone();
        let working_directory = self.provider.working_directory.clone();
        let timeout_ms = self.provider.timeout_ms;
        self.task = Some(tokio::spawn(async move {
            process::run_command(
                &root,
                &argv,
                &environment,
                working_directory.as_deref(),
                timeout_ms,
            )
            .await
        }));
        self.wait_for_endpoint(profile.port, timeout_ms).await?;
        let receipt = DiagnosticReceipt {
            version: DIAGNOSTIC_RECEIPT_VERSION,
            receipt_id: random_id("diag")?,
            occurrence_id: occurrence_id.to_string(),
            run_id: self.run_id.clone(),
            cell_receipt_id: self.receipt_id.clone(),
            debugger: profile.debugger,
            endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port: profile.port,
            },
            source_mappings: vec![SourceMapping {
                local_root: self
                    .root
                    .join(profile.local_source_root)
                    .display()
                    .to_string(),
                target_root: profile.target_source_root.display().to_string(),
            }],
            pause_point: "before-trigger".into(),
            perturbations: vec![
                "provider-debug-command-override".into(),
                "attach-before-trigger-pause".into(),
            ],
            authoritative: false,
        };
        receipt.validate().map_err(|error| anyhow::anyhow!(error))?;
        write_debugger_files(&self.root, &self.run_id, &receipt)?;
        Ok(receipt)
    }

    pub(super) fn owns(&self, provider_id: &str) -> bool {
        self.provider_id == provider_id
    }

    pub(super) async fn finish_trigger(
        &mut self,
        expected_identity: &str,
    ) -> Result<(ProviderRun, ProviderVerdict)> {
        let result = self
            .task
            .take()
            .context("host debug command was not started")?
            .await
            .context("joining host debug command")??;
        process::evaluate_provider_result(
            &self.root,
            &self.provider_id,
            &self.provider,
            expected_identity,
            result,
        )
        .await
    }

    pub(super) async fn cleanup(
        &mut self,
        state_fingerprints: BTreeMap<String, String>,
    ) -> Result<CellReceipt> {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        let receipt = CellReceipt {
            version: CELL_RECEIPT_VERSION,
            receipt_id: self.receipt_id.clone(),
            run_id: self.run_id.clone(),
            cell_id: self.provider_id.clone(),
            driver: "local-process".into(),
            project_name: self.run_id.clone(),
            configuration_sha256: self.configuration_sha256.clone(),
            services: vec![self.provider_id.clone()],
            state_fingerprints,
            cleanup: CleanupStatus::Attempted,
            missing_capabilities: vec!["descendant-process-cleanup-unverified".into()],
        };
        receipt.validate().map_err(|error| anyhow::anyhow!(error))?;
        Ok(receipt)
    }

    async fn wait_for_endpoint(&self, port: u16, timeout_ms: u64) -> Result<()> {
        let attempts = (timeout_ms / 100).clamp(1, 600);
        for _ in 0..attempts {
            if self.task.as_ref().is_some_and(JoinHandle::is_finished) {
                anyhow::bail!("debug command exited before opening its loopback endpoint");
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("debug endpoint 127.0.0.1:{port} was not ready within the provider timeout")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution::ExecutionPhase;
    use reproit_protocol::{
        MechanismAuthority, ObservationAuthority, ObservationKind, ObservationTarget, PlanBinding,
        PLAN_VERSION,
    };
    use std::collections::BTreeSet;

    #[test]
    fn host_debug_fixture_process() {
        if std::env::var("REPROIT_HOST_DEBUG_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let port = std::env::var("REPROIT_HOST_DEBUG_FIXTURE_PORT")
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        for _ in 0..2 {
            let _ = listener.accept().unwrap();
        }
        std::process::exit(17);
    }

    #[tokio::test]
    async fn trusted_host_provider_attaches_before_its_trigger() {
        let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let root =
            std::env::temp_dir().join(format!("reproit-host-debug-{}", random_id("test").unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = CommandProvider {
            authority: MechanismAuthority::TrustedCheckout,
            phase: ExecutionPhase::Trigger,
            capabilities: BTreeSet::new(),
            source: None,
            cell: None,
            debug: Some(super::super::model::DebugProfile {
                debugger: reproit_protocol::DebuggerKind::LanguageSpecific,
                argv: vec![
                    std::env::current_exe().unwrap().display().to_string(),
                    "--exact".into(),
                    "adapters::execution::runner::host_debug::tests::host_debug_fixture_process"
                        .into(),
                    "--nocapture".into(),
                ],
                port,
                local_source_root: ".".into(),
                target_source_root: root.clone(),
            }),
            argv: vec!["unused-authoritative-command".into()],
            environment: BTreeMap::from([
                ("REPROIT_HOST_DEBUG_FIXTURE".into(), "1".into()),
                ("REPROIT_HOST_DEBUG_FIXTURE_PORT".into(), port.to_string()),
            ]),
            working_directory: None,
            timeout_ms: 5_000,
            clean_exit_codes: vec![0],
            observation: Some(super::super::model::CommandObservation {
                identity: "fixture-exit".into(),
                matcher: super::super::model::ObservationMatcher::ExitCode { code: 17 },
            }),
            state_fingerprint: None,
            cleanup: None,
        };
        let plan = ReproductionPlan {
            version: PLAN_VERSION,
            id: "plan_fixture".into(),
            occurrence_id: "occ_fixture".into(),
            target: "current-checkout".into(),
            destination: reproit_protocol::ExecutionDestination::LocalProcess,
            bindings: vec![PlanBinding {
                requirement_id: "req_fixture".into(),
                provider_id: "fixture".into(),
                mechanism_authority: provider.authority,
                template_digest: catalog::provider_digest(&provider).unwrap(),
                evidence_artifact_ids: Vec::new(),
            }],
            observation: ObservationTarget {
                observation: ObservationKind::Exit,
                identity: "fixture-exit".into(),
                authority: ObservationAuthority::RuntimeDiagnosis,
            },
        };
        let providers = vec![("fixture".into(), &provider)];
        let mut session = HostDebugSession::prepare(&root, &plan, &providers).unwrap();
        let receipt = session.start("occ_fixture").await.unwrap();
        tokio::net::TcpStream::connect((receipt.endpoint.host.as_str(), receipt.endpoint.port))
            .await
            .unwrap();
        let (run, verdict) = session.finish_trigger("fixture-exit").await.unwrap();
        assert!(run.observation_matched);
        assert_eq!(verdict, ProviderVerdict::Reproduced);
        let cell = session.cleanup(BTreeMap::new()).await.unwrap();
        assert_eq!(cell.cleanup, CleanupStatus::Attempted);
        cell.validate().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
