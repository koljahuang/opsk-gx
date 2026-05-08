#!/bin/bash
#
# Install EKS cluster add-ons via Helm
# Prerequisite: EKS cluster created, IAM roles and Pod Identity Associations exist
#
# Environment variables:
#   ONLY=alloy,argo-rollouts   Install only specified components (comma-separated)
#   FORCE=true                 Force reinstall even if version matches
#   SKIP_REPO_UPDATE=true      Skip `helm repo update` (faster repeated runs)
#   SKIP_OBSERVABILITY=true    Skip Mimir/Loki/Tempo (Grafana Cloud mode)
#   SKIP_ALBC=true             Skip AWS Load Balancer Controller
#   SKIP_KARPENTER=true        Skip Karpenter
#   SKIP_ARGOCD=true           Skip ArgoCD
#   SKIP_ARGO_ROLLOUTS=true    Skip Argo Rollouts
#
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
GRAY='\033[0;90m'
NC='\033[0m'

log()  { echo -e "${GREEN}[helm-install]${NC} $*"; }
warn() { echo -e "${YELLOW}[helm-install]${NC} $*"; }
err()  { echo -e "${RED}[helm-install]${NC} $*" >&2; }
step() { echo -e "\n${BLUE}── $1${NC}"; }
skip() { echo -e "${GRAY}[helm-install] $1 — skipped${NC}"; }

# Check if a Helm release is already installed at the target chart version
# Usage: is_installed <release> <namespace> <chart-version>
is_installed() {
    [[ "$FORCE" == "true" ]] && return 1
    local release=$1 ns=$2 target_version=$3
    local current
    current=$(helm list -n "$ns" -f "^${release}$" -o json 2>/dev/null \
        | jq -r '.[0].chart // empty' 2>/dev/null)
    [[ -n "$current" && "${current##*-}" == "$target_version" ]]
}

# Check if a component should be installed (ONLY= filter)
# Usage: should_install <component-name>
should_install() {
    [[ -z "$ONLY" ]] && return 0
    [[ ",$ONLY," == *",$1,"* ]]
}

# Check dependencies
for cmd in kubectl helm jq; do
    if ! command -v "$cmd" &>/dev/null; then
        err "$cmd is required but not found"
        exit 1
    fi
done

# Add Helm repos (only if we'll actually install something)
if [[ "$SKIP_REPO_UPDATE" != "true" ]]; then
    step "Adding Helm repositories"
    helm repo add eks https://aws.github.io/eks-charts 2>/dev/null || true
    helm repo add metrics-server https://kubernetes-sigs.github.io/metrics-server/ 2>/dev/null || true
    helm repo add external-secrets https://charts.external-secrets.io 2>/dev/null || true
    helm repo add grafana https://grafana.github.io/helm-charts 2>/dev/null || true
    helm repo add prometheus-community https://prometheus-community.github.io/helm-charts 2>/dev/null || true
    helm repo add argo https://argoproj.github.io/argo-helm 2>/dev/null || true
    helm repo update
else
    skip "Helm repo update (SKIP_REPO_UPDATE=true)"
fi

# Create gp3 StorageClass (default) — always runs, fast & idempotent
if should_install "storageclass"; then
    step "Creating gp3 StorageClass"
    kubectl annotate storageclass gp2 storageclass.kubernetes.io/is-default-class=false --overwrite 2>/dev/null || true
    cat <<EOF | kubectl apply -f -
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: gp3
  annotations:
    storageclass.kubernetes.io/is-default-class: "true"
provisioner: ebs.csi.aws.com
parameters:
  type: gp3
  encrypted: "true"
reclaimPolicy: Delete
volumeBindingMode: WaitForFirstConsumer
allowVolumeExpansion: true
EOF
    log "gp3 StorageClass created"
fi

# AWS Load Balancer Controller
if [[ "$SKIP_ALBC" != "true" ]] && should_install "albc"; then
    if is_installed aws-load-balancer-controller kube-system "3.2.1"; then
        skip "AWS Load Balancer Controller 3.2.1 ✓"
    else
        step "Installing AWS Load Balancer Controller"
        helm upgrade --install aws-load-balancer-controller eks/aws-load-balancer-controller \
            --version "3.2.1" \
            -n kube-system \
            -f "$SCRIPT_DIR/aws-load-balancer-controller-values.yaml" \
            --timeout 600s \
            --wait
        log "AWS Load Balancer Controller installed"
    fi
fi

# Karpenter
if [[ "$SKIP_KARPENTER" != "true" && -f "$SCRIPT_DIR/karpenter-values.yaml" ]] && should_install "karpenter"; then
    if is_installed karpenter kube-system "1.11.1"; then
        skip "Karpenter 1.11.1 ✓"
    else
        step "Installing Karpenter"
        helm upgrade --install karpenter oci://public.ecr.aws/karpenter/karpenter \
            --version "1.11.1" \
            -n kube-system \
            -f "$SCRIPT_DIR/karpenter-values.yaml" \
            --timeout 600s \
            --wait
        log "Karpenter installed"
    fi

    # Apply Karpenter node configuration
    if [[ -x "$SCRIPT_DIR/../karpenter/apply-karpenter-config.sh" ]]; then
        "$SCRIPT_DIR/../karpenter/apply-karpenter-config.sh"
    fi
fi

# Metrics Server
if should_install "metrics-server"; then
    if is_installed metrics-server kube-system "3.13.0"; then
        skip "Metrics Server 3.13.0 ✓"
    else
        step "Installing Metrics Server"
        helm upgrade --install metrics-server metrics-server/metrics-server \
            --version "3.13.0" \
            -n kube-system \
            -f "$SCRIPT_DIR/metrics-server-values.yaml" \
            --timeout 600s \
            --wait
        log "Metrics Server installed"
    fi
fi

# External Secrets Operator
if should_install "external-secrets"; then
    if is_installed external-secrets external-secrets "2.3.0"; then
        skip "External Secrets Operator 2.3.0 ✓"
    else
        step "Installing External Secrets Operator"
        helm upgrade --install external-secrets external-secrets/external-secrets \
            --version "2.3.0" \
            -n external-secrets --create-namespace \
            -f "$SCRIPT_DIR/external-secrets-values.yaml" \
            --timeout 600s \
            --wait
        log "External Secrets Operator installed"
    fi
fi

# Monitoring namespace (needed by both self-hosted observability and Alloy)
step "Creating monitoring namespace"
kubectl create namespace monitoring --dry-run=client -o yaml | kubectl apply -f -

# Apply observability ExternalSecret (Grafana Cloud credentials for Alloy)
INFRA_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
if [[ -f "$INFRA_DIR/observability-external-secret.yaml" ]]; then
    step "Applying observability ExternalSecret (monitoring namespace)"
    kubectl apply -f "$INFRA_DIR/observability-external-secret.yaml"
    log "Observability ExternalSecret applied"
fi

# Self-hosted observability backends (Mimir + Loki + Tempo)
# Skip with SKIP_OBSERVABILITY=true when using Grafana Cloud
if [[ "$SKIP_OBSERVABILITY" != "true" ]]; then
    # Mimir
    if should_install "mimir"; then
        if is_installed mimir monitoring "6.0.6"; then
            skip "Mimir 6.0.6 ✓"
        else
            step "Installing Mimir (metrics)"
            helm upgrade --install mimir grafana/mimir-distributed \
                --version "6.0.6" \
                -n monitoring \
                -f "$SCRIPT_DIR/mimir-values.yaml" \
                --timeout 600s \
                --wait
            log "Mimir installed"
        fi
    fi

    # Loki
    if should_install "loki"; then
        if is_installed loki monitoring "6.55.0"; then
            skip "Loki 6.55.0 ✓"
        else
            step "Installing Loki (logs)"
            helm upgrade --install loki grafana/loki \
                --version "6.55.0" \
                -n monitoring \
                -f "$SCRIPT_DIR/loki-values.yaml" \
                --timeout 600s \
                --wait
            log "Loki installed"
        fi
    fi

    # Tempo
    if should_install "tempo"; then
        if is_installed tempo monitoring "1.24.4"; then
            skip "Tempo 1.24.4 ✓"
        else
            step "Installing Tempo (traces)"
            helm upgrade --install tempo grafana/tempo \
                --version "1.24.4" \
                -n monitoring \
                -f "$SCRIPT_DIR/tempo-values.yaml" \
                --timeout 600s \
                --wait
            log "Tempo installed"
        fi
    fi
else
    warn "Skipping self-hosted observability backends (SKIP_OBSERVABILITY=true)"
fi

# Alloy collector + kube-state-metrics — always installed (works with both self-hosted and Grafana Cloud)
if should_install "kube-state-metrics"; then
    if is_installed kube-state-metrics monitoring "7.2.2"; then
        skip "kube-state-metrics 7.2.2 ✓"
    else
        step "Installing kube-state-metrics"
        helm upgrade --install kube-state-metrics prometheus-community/kube-state-metrics \
            --version "7.2.2" \
            -n monitoring \
            --set nodeSelector."karpenter\.sh/nodepool"=common-nodepool \
            --timeout 300s \
            --wait
        log "kube-state-metrics installed"
    fi
fi

if should_install "alloy"; then
    if is_installed alloy monitoring "1.7.0"; then
        skip "Alloy 1.7.0 ✓"
    else
        step "Installing Alloy (collector)"
        helm upgrade --install alloy grafana/alloy \
            --version "1.7.0" \
            -n monitoring \
            -f "$SCRIPT_DIR/alloy-values.yaml" \
            --timeout 600s \
            --wait
        log "Alloy installed"
    fi
fi

# ArgoCD
if [[ "$SKIP_ARGOCD" != "true" ]] && should_install "argocd"; then
    if is_installed argocd argocd "9.5.0"; then
        skip "ArgoCD 9.5.0 ✓"
    else
        step "Installing ArgoCD"
        kubectl create namespace argocd --dry-run=client -o yaml | kubectl apply -f -
        helm upgrade --install argocd argo/argo-cd \
            --version "9.5.0" \
            -n argocd \
            -f "$SCRIPT_DIR/argocd-values.yaml" \
            --timeout 600s \
            --wait
        log "ArgoCD installed"
    fi

    # Print initial admin password
    ARGOCD_PASS=$(kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath="{.data.password}" 2>/dev/null | base64 -d 2>/dev/null || echo "")
    if [[ -n "$ARGOCD_PASS" ]]; then
        log "ArgoCD admin password: $ARGOCD_PASS"
        log "Access: kubectl port-forward svc/argocd-server -n argocd 8080:443"
    fi
fi

# Argo Rollouts
if [[ "$SKIP_ARGO_ROLLOUTS" != "true" ]] && should_install "argo-rollouts"; then
    if is_installed argo-rollouts argo-rollouts "2.40.9"; then
        skip "Argo Rollouts 2.40.9 ✓"
    else
        step "Installing Argo Rollouts"
        kubectl create namespace argo-rollouts --dry-run=client -o yaml | kubectl apply -f -
        helm upgrade --install argo-rollouts argo/argo-rollouts \
            --version "2.40.9" \
            -n argo-rollouts \
            -f "$SCRIPT_DIR/argo-rollouts-values.yaml" \
            --timeout 600s \
            --wait
        log "Argo Rollouts installed"
    fi
fi

echo ""
log "All Helm installations complete!"
