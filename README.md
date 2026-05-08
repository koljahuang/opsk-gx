# OpsK-GX

Multi-cloud infrastructure operations platform with AI-powered Root Cause Analysis.

## Demo

https://github.com/user-attachments/assets/9c6ceea6-ff79-456d-8857-bd70a4461afc

## Tech Stack

- **Frontend**: Nuxt 3 + shadcn-vue + Tailwind CSS (TypeScript)
- **Backend**: Rust + Axum + SQLx + PostgreSQL
- **AI**: Claude CLI with MCP tools for automated investigation
- **Infrastructure**: EKS + Karpenter (ARM64 Graviton), Helm, ArgoCD
- **Observability**: Grafana / Mimir / Loki / Tempo

## Project Structure

```
opsk-gx/
├── frontend/          # Nuxt 3 SSR application
├── backend/           # Rust API server
├── k8s/               # Kubernetes manifests + Helm charts
├── scripts/           # Deploy, build, and dev scripts
├── docker-compose.yml # Local dev (PostgreSQL + Redis)
├── Dockerfile.backend
└── Dockerfile.frontend
```

## Getting Started

### Prerequisites

- Rust 1.75+
- Node.js 20+
- Docker & Docker Compose
- PostgreSQL 15+

### Local Development

```bash
# Start dependencies (PostgreSQL + Redis)
docker compose up -d

# Start backend (port 3080)
cd backend && cargo run

# Start frontend (port 3000)
cd frontend && npm install && npm run dev
```

Or use the one-click script:

```bash
./scripts/local-dev.sh
```

## Deployment

```bash
./scripts/deploy-all.sh           # Full deployment
./scripts/deploy-to-existing.sh   # App-only deploy to existing cluster
./scripts/destroy.sh              # Tear down
```

## Key Features

- Multi-cloud account management (AWS, Alicloud, Azure)
- EKS cluster discovery and management
- Argo Rollouts integration (canary/bluegreen)
- AI-powered Root Cause Analysis with real-time streaming
- Issue tracking with severity and lifecycle management
- Multi-tenant isolation with RBAC
- Observability integration (metrics, logs, traces)

## License

See [LICENSE](./LICENSE).
