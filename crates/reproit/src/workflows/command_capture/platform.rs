//! Zero-configuration platform evidence collectors.
//!
//! Every adapter is observational. It reads documented metadata surfaces and
//! emits a typed identity only when all identity fields are available.

use reproit_protocol::{
    BuildIdentity, PlatformEvidence, PlatformIdentity, ResourceLimits, WorkloadIdentity,
    PLATFORM_EVIDENCE_VERSION,
};
use serde_json::Value;
use std::time::Duration;

const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 16 * 1024;

pub(super) struct Collection {
    pub(super) evidence: Vec<PlatformEvidence>,
    pub(super) gaps: Vec<String>,
}

pub(super) async fn collect(component: &str) -> Collection {
    let mut collection = Collection {
        evidence: Vec::new(),
        gaps: Vec::new(),
    };
    collect_kubernetes(component, &mut collection);
    collect_docker_compose(component, &mut collection);
    collect_ecs(component, &mut collection).await;
    collect_serverless(component, &mut collection);
    collect_native_service(component, &mut collection);
    collect_ci(component, &mut collection);
    collect_android(component, &mut collection);
    collect_ios(component, &mut collection);
    collection
}

fn collect_kubernetes(component: &str, collection: &mut Collection) {
    if env("KUBERNETES_SERVICE_HOST").is_none() {
        return;
    }
    let namespace = env("REPROIT_K8S_NAMESPACE")
        .or_else(|| env("POD_NAMESPACE"))
        .or_else(read_service_account_namespace);
    let workload_kind = env("REPROIT_K8S_WORKLOAD_KIND");
    let workload_name = env("REPROIT_K8S_WORKLOAD_NAME");
    let pod_uid = env("REPROIT_K8S_POD_UID");
    let (Some(namespace), Some(workload_kind), Some(workload_name), Some(pod_uid)) =
        (namespace, workload_kind, workload_name, pod_uid)
    else {
        collection.gaps.push(
            "kubernetes workload identity needs namespace, workload kind/name, and pod UID from the downward API"
                .into(),
        );
        return;
    };
    collection.evidence.push(evidence(
        PlatformIdentity::Kubernetes {
            namespace,
            workload_kind,
            workload_name: workload_name.clone(),
            pod_uid: pod_uid.clone(),
            cluster: env("REPROIT_K8S_CLUSTER"),
            container: env("REPROIT_K8S_CONTAINER"),
        },
        component,
        Some(workload_name),
        Some(pod_uid),
        Vec::new(),
    ));
}

fn collect_docker_compose(component: &str, collection: &mut Collection) {
    let project = env("COMPOSE_PROJECT_NAME");
    let service = env("REPROIT_COMPOSE_SERVICE").or_else(|| env("COMPOSE_SERVICE"));
    if project.is_none() && service.is_none() {
        return;
    }
    let container_id = env("REPROIT_CONTAINER_ID").or_else(|| env("HOSTNAME"));
    let (Some(project), Some(service), Some(container_id)) = (project, service, container_id)
    else {
        collection
            .gaps
            .push("Compose identity needs project, service, and container ID".into());
        return;
    };
    collection.evidence.push(evidence(
        PlatformIdentity::DockerCompose {
            project,
            service: service.clone(),
            container_id: container_id.clone(),
        },
        component,
        Some(service),
        Some(container_id),
        Vec::new(),
    ));
}

async fn collect_ecs(component: &str, collection: &mut Collection) {
    let Some(base) = env("ECS_CONTAINER_METADATA_URI_V4") else {
        return;
    };
    match ecs_task_metadata(&base).await {
        Ok(metadata) => {
            let cluster = string_at(&metadata, "Cluster");
            let task_arn = string_at(&metadata, "TaskARN");
            let container = metadata
                .get("Containers")
                .and_then(Value::as_array)
                .and_then(|containers| containers.first())
                .and_then(|container| container.get("Name"))
                .and_then(Value::as_str)
                .and_then(bounded);
            let instance = metadata
                .get("Containers")
                .and_then(Value::as_array)
                .and_then(|containers| containers.first())
                .and_then(|container| container.get("DockerId"))
                .and_then(Value::as_str)
                .and_then(bounded);
            let (Some(cluster), Some(task_arn), Some(container)) = (cluster, task_arn, container)
            else {
                collection
                    .gaps
                    .push("ECS task metadata omitted cluster, task ARN, or container name".into());
                return;
            };
            collection.evidence.push(evidence(
                PlatformIdentity::Ecs {
                    cluster,
                    task_arn: task_arn.clone(),
                    container,
                },
                component,
                Some(task_arn),
                instance,
                Vec::new(),
            ));
        }
        Err(detail) => collection
            .gaps
            .push(format!("ECS metadata unavailable: {detail}")),
    }
}

async fn ecs_task_metadata(base: &str) -> Result<Value, String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| "invalid metadata URI")?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("169.254.170.2" | "127.0.0.1" | "::1"))
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("metadata URI is not an approved link-local or loopback endpoint".into());
    }
    let path = format!("{}/task", url.path().trim_end_matches('/'));
    url.set_path(&path);
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "metadata client could not be built")?
        .get(url)
        .send()
        .await
        .map_err(|_| "metadata request failed")?;
    if !response.status().is_success() {
        return Err(format!("metadata returned HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "metadata body could not be read")?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err("metadata response exceeded 64 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "metadata response was not valid JSON".into())
}

fn collect_serverless(component: &str, collection: &mut Collection) {
    let identity = if let Some(function) = env("AWS_LAMBDA_FUNCTION_NAME") {
        Some(PlatformIdentity::Serverless {
            provider: "aws-lambda".into(),
            function,
            region: env("AWS_REGION"),
            instance: env("AWS_LAMBDA_LOG_STREAM_NAME"),
        })
    } else if let Some(function) = env("K_SERVICE") {
        Some(PlatformIdentity::Serverless {
            provider: "google-cloud-run".into(),
            function,
            region: env("GOOGLE_CLOUD_REGION"),
            instance: env("K_REVISION"),
        })
    } else {
        env("WEBSITE_SITE_NAME").map(|function| PlatformIdentity::Serverless {
            provider: "azure-functions".into(),
            function,
            region: env("REGION_NAME"),
            instance: env("WEBSITE_INSTANCE_ID"),
        })
    };
    if let Some(identity) = identity {
        collection
            .evidence
            .push(evidence(identity, component, None, None, Vec::new()));
    }
}

fn collect_native_service(component: &str, collection: &mut Collection) {
    let identity = if let Some(instance) = env("INVOCATION_ID") {
        env("REPROIT_SYSTEMD_UNIT").map(|service| PlatformIdentity::NativeService {
            operating_system: "linux".into(),
            service_manager: "systemd".into(),
            service,
            instance: Some(instance),
        })
    } else if let Some(service) = env("XPC_SERVICE_NAME").filter(|service| service != "0") {
        Some(PlatformIdentity::NativeService {
            operating_system: "macos".into(),
            service_manager: "launchd".into(),
            service,
            instance: None,
        })
    } else {
        env("REPROIT_WINDOWS_SERVICE_NAME").map(|service| PlatformIdentity::NativeService {
            operating_system: "windows".into(),
            service_manager: "service-control-manager".into(),
            service,
            instance: env("REPROIT_WINDOWS_SERVICE_INSTANCE"),
        })
    };
    if env("INVOCATION_ID").is_some() && identity.is_none() {
        collection
            .gaps
            .push("systemd identity needs REPROIT_SYSTEMD_UNIT".into());
    }
    if let Some(identity) = identity {
        collection
            .evidence
            .push(evidence(identity, component, None, None, Vec::new()));
    }
}

fn collect_ci(component: &str, collection: &mut Collection) {
    let identity = if env("GITHUB_ACTIONS").as_deref() == Some("true") {
        zip3(env("GITHUB_RUN_ID"), env("GITHUB_JOB"), |pipeline, job| {
            PlatformIdentity::Ci {
                provider: "github-actions".into(),
                pipeline,
                job,
                runner: env("RUNNER_NAME"),
            }
        })
    } else if env("GITLAB_CI").as_deref() == Some("true") {
        zip3(env("CI_PIPELINE_ID"), env("CI_JOB_ID"), |pipeline, job| {
            PlatformIdentity::Ci {
                provider: "gitlab-ci".into(),
                pipeline,
                job,
                runner: env("CI_RUNNER_ID"),
            }
        })
    } else if env("BUILDKITE").as_deref() == Some("true") {
        zip3(
            env("BUILDKITE_BUILD_ID"),
            env("BUILDKITE_JOB_ID"),
            |pipeline, job| PlatformIdentity::Ci {
                provider: "buildkite".into(),
                pipeline,
                job,
                runner: env("BUILDKITE_AGENT_ID"),
            },
        )
    } else if env("CIRCLECI").as_deref() == Some("true") {
        zip3(
            env("CIRCLE_WORKFLOW_ID"),
            env("CIRCLE_BUILD_NUM"),
            |pipeline, job| PlatformIdentity::Ci {
                provider: "circleci".into(),
                pipeline,
                job,
                runner: env("CIRCLE_NODE_INDEX"),
            },
        )
    } else {
        None
    };
    if let Some(identity) = identity {
        collection
            .evidence
            .push(evidence(identity, component, None, None, Vec::new()));
    }
}

fn collect_android(component: &str, collection: &mut Collection) {
    let Some(serial) = env("ANDROID_SERIAL") else {
        return;
    };
    collection.evidence.push(evidence(
        PlatformIdentity::Android {
            serial,
            api_level: env("REPROIT_ANDROID_API_LEVEL"),
            architecture: env("REPROIT_ANDROID_ARCH"),
            application_id: env("REPROIT_ANDROID_APPLICATION_ID"),
        },
        component,
        None,
        None,
        missing(&[
            ("android API level", "REPROIT_ANDROID_API_LEVEL"),
            ("Android architecture", "REPROIT_ANDROID_ARCH"),
            ("Android application ID", "REPROIT_ANDROID_APPLICATION_ID"),
        ]),
    ));
}

fn collect_ios(component: &str, collection: &mut Collection) {
    let Some(udid) = env("SIMULATOR_UDID").or_else(|| env("REPROIT_IOS_UDID")) else {
        return;
    };
    collection.evidence.push(evidence(
        PlatformIdentity::Ios {
            udid,
            runtime: env("SIMULATOR_RUNTIME_VERSION").or_else(|| env("REPROIT_IOS_RUNTIME")),
            device_type: env("SIMULATOR_MODEL_IDENTIFIER")
                .or_else(|| env("REPROIT_IOS_DEVICE_TYPE")),
            bundle_id: env("REPROIT_IOS_BUNDLE_ID"),
        },
        component,
        None,
        None,
        missing(&[
            ("iOS runtime", "REPROIT_IOS_RUNTIME"),
            ("iOS device type", "REPROIT_IOS_DEVICE_TYPE"),
            ("iOS bundle ID", "REPROIT_IOS_BUNDLE_ID"),
        ]),
    ));
}

fn evidence(
    platform: PlatformIdentity,
    service: &str,
    workload: Option<String>,
    instance: Option<String>,
    missing_capabilities: Vec<String>,
) -> PlatformEvidence {
    PlatformEvidence {
        version: PLATFORM_EVIDENCE_VERSION,
        collector: "reproit-platform".into(),
        platform,
        workload: Some(WorkloadIdentity {
            service: service.to_string(),
            workload,
            instance,
        }),
        build: build_identity(),
        resources: resource_limits(),
        missing_capabilities,
    }
}

fn build_identity() -> Option<BuildIdentity> {
    let image_digest = env("REPROIT_IMAGE_DIGEST");
    let artifact_digest = env("REPROIT_ARTIFACT_DIGEST");
    (image_digest.is_some() || artifact_digest.is_some()).then_some(BuildIdentity {
        image_digest,
        artifact_digest,
    })
}

fn resource_limits() -> Option<ResourceLimits> {
    let cpu_millis = number("REPROIT_CPU_LIMIT_MILLIS");
    let memory_bytes = number("REPROIT_MEMORY_LIMIT_BYTES");
    let ephemeral_storage_bytes = number("REPROIT_STORAGE_LIMIT_BYTES");
    (cpu_millis.is_some() || memory_bytes.is_some() || ephemeral_storage_bytes.is_some()).then_some(
        ResourceLimits {
            cpu_millis,
            memory_bytes,
            ephemeral_storage_bytes,
        },
    )
}

fn missing(fields: &[(&str, &str)]) -> Vec<String> {
    fields
        .iter()
        .filter(|(_, variable)| env(variable).is_none())
        .map(|(label, variable)| format!("{label} was not captured; provide {variable}"))
        .collect()
}

fn zip3(
    first: Option<String>,
    second: Option<String>,
    build: impl FnOnce(String, String) -> PlatformIdentity,
) -> Option<PlatformIdentity> {
    Some(build(first?, second?))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| bounded(value.trim()))
}

fn bounded(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= MAX_FIELD_BYTES).then(|| value.to_string())
}

fn number(name: &str) -> Option<u64> {
    env(name)?.parse().ok().filter(|value| *value > 0)
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().and_then(bounded)
}

fn read_service_account_namespace() -> Option<String> {
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .ok()
        .and_then(|value| bounded(value.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use tokio::sync::{Mutex, MutexGuard};

    async fn environment_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    #[tokio::test]
    async fn collectors_report_multiple_nested_platforms_without_guessing() {
        let _guard = environment_lock().await;
        let variables = [
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_RUN_ID", "run-1"),
            ("GITHUB_JOB", "test"),
            ("ANDROID_SERIAL", "emulator-5554"),
        ];
        let previous = variables
            .iter()
            .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        for (name, value) in variables {
            std::env::set_var(name, value);
        }
        let collected = collect("checkout").await;
        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        assert!(collected
            .evidence
            .iter()
            .any(|value| matches!(value.platform, PlatformIdentity::Ci { .. })));
        assert!(collected
            .evidence
            .iter()
            .any(|value| matches!(value.platform, PlatformIdentity::Android { .. })));
        for evidence in collected.evidence {
            evidence.validate().unwrap();
        }
    }
}
