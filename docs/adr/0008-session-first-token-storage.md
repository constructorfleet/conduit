# Session-First Token Storage

The operator UI will keep management bearer tokens in browser session memory by default and only persist them locally after an explicit operator choice. Management tokens can read events and manage pipelines, so silent long-lived browser storage would trade away security without the operator knowingly accepting the convenience.
