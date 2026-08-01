# Frontend Shares Conduit Release Train

The operator UI will live in the Conduit repository and share Conduit's release train while keeping its own frontend package and toolchain. The UI depends closely on the service API and event vocabulary, so separate versioning would add compatibility overhead before the project needs it; the frontend can be split later if operational complexity justifies it.
