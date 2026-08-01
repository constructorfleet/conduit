# Conduit Operator Console

React, TypeScript, and Vite frontend for Conduit's Operator Console.

## Commands

```sh
npm ci
npm run dev
npm run lint
npm run test
npm run build
npm run format
```

The access foundation uses management bearer tokens or explicit anonymous mode.
Bearer tokens stay in `sessionStorage` unless the operator chooses
`Remember on this browser`, which writes to `localStorage`.

The app shell is organized around Overview, Pipelines, Providers, Events, and
Settings. Runtime integration starts from `/v1/status` and then applies
`/v1/events` updates after the snapshot is loaded.
