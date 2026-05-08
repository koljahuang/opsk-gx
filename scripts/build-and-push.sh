#!/bin/bash
#
# Ops - Build Docker images and push to ECR
#
# Usage:
#   ./build-and-push.sh               # Build and push both
#   ./build-and-push.sh --backend     # Build and push backend only
#   ./build-and-push.sh --frontend    # Build and push frontend only
#   ./build-and-push.sh --cn          # Use China mirrors (apt/pip/npm)
#
set -e

export AWS_PAGER=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
IAC_DIR="$PROJECT_ROOT/iac"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

log()  { echo -e "${GREEN}[build]${NC} $*"; }
warn() { echo -e "${YELLOW}[build]${NC} $*"; }
err()  { echo -e "${RED}[build]${NC} $*" >&2; }

BUILD_BACKEND=true
BUILD_FRONTEND=true
CN_MIRROR=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --backend)  BUILD_FRONTEND=false; shift ;;
        --frontend) BUILD_BACKEND=false; shift ;;
        --cn)       CN_MIRROR=1; shift ;;
        *) err "Unknown: $1"; exit 1 ;;
    esac
done

# Get AWS info
AWS_REGION=$(cd "$IAC_DIR" && terraform output -raw region 2>/dev/null || echo "${AWS_REGION:-us-east-1}")
AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
ECR_BASE="${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com"
GIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "latest")

log "ECR: $ECR_BASE"
log "Git SHA: $GIT_SHA"

# Login to ECR (private — for push)
log "Logging into ECR..."
aws ecr get-login-password --region "$AWS_REGION" | docker login --username AWS --password-stdin "$ECR_BASE"

# Login to ECR Public (for pulling base images)
log "Logging into ECR Public..."
aws ecr-public get-login-password --region us-east-1 2>/dev/null | docker login --username AWS --password-stdin public.ecr.aws || {
    warn "ECR Public login failed — base image pulls may be rate-limited"
}

# Ensure ECR repositories exist
for repo in opsk-backend opsk-frontend; do
    aws ecr describe-repositories --repository-names "$repo" --region "$AWS_REGION" &>/dev/null || \
        aws ecr create-repository --repository-name "$repo" --region "$AWS_REGION" --image-scanning-configuration scanOnPush=true
done

# Build backend
if [[ "$BUILD_BACKEND" == "true" ]]; then
    log "Building backend image..."
    docker build --build-arg CN_MIRROR="$CN_MIRROR" -f "$PROJECT_ROOT/Dockerfile.backend" -t "$ECR_BASE/opsk-backend:latest" -t "$ECR_BASE/opsk-backend:$GIT_SHA" "$PROJECT_ROOT"
    docker push "$ECR_BASE/opsk-backend:latest"
    docker push "$ECR_BASE/opsk-backend:$GIT_SHA"
    log "Backend pushed: $ECR_BASE/opsk-backend:$GIT_SHA"
fi

# Build frontend
if [[ "$BUILD_FRONTEND" == "true" ]]; then
    log "Building frontend image..."
    docker build --build-arg CN_MIRROR="$CN_MIRROR" -f "$PROJECT_ROOT/Dockerfile.frontend" -t "$ECR_BASE/opsk-frontend:latest" -t "$ECR_BASE/opsk-frontend:$GIT_SHA" "$PROJECT_ROOT"
    docker push "$ECR_BASE/opsk-frontend:latest"
    docker push "$ECR_BASE/opsk-frontend:$GIT_SHA"
    log "Frontend pushed: $ECR_BASE/opsk-frontend:$GIT_SHA"
fi

log "Build and push complete!"
