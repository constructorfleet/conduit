"""Worked example: wiring Vox against the shared `conduit_link` skeleton.

Shows how a service supplies its extension dataclass, config, client, and the
three callbacks the router factory needs. Type-checks against the skeleton
API; no runtime logic asserted.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Mapping

from fastapi import FastAPI

from conduit_link import (
    ConduitLinkClient,
    HttpConduitLinkClient,
    LinkConfig,
    LinkRequest,
    LinkStore,
    LinkedServiceKind,
    LinkedServicePanel,
    make_link_router,
)


@dataclass(frozen=True)
class VoxLinkExtension:
    provider_definition_id: str
    local_api_key: str


def _ext_from_dict(payload: Mapping[str, object]) -> VoxLinkExtension:
    return VoxLinkExtension(
        provider_definition_id=str(payload["provider_definition_id"]),
        local_api_key=str(payload["local_api_key"]),
    )


def _ext_to_dict(extension: VoxLinkExtension) -> dict[str, object]:
    return {
        "provider_definition_id": extension.provider_definition_id,
        "local_api_key": extension.local_api_key,
    }


def _build_create_body(
    request: LinkRequest, existing: VoxLinkExtension | None
) -> Mapping[str, object]:
    return {
        "peer_name": request.peer_name,
        "vox_base_url": "http://vox.local",
    }


def _build_extension(
    request: LinkRequest,
    response: Mapping[str, str],
    existing: VoxLinkExtension | None,
) -> VoxLinkExtension:
    return VoxLinkExtension(
        provider_definition_id=response["provider_definition_id"],
        local_api_key=existing.local_api_key if existing else "generated",
    )


def _public(extension: VoxLinkExtension) -> Mapping[str, object]:
    return {"provider_definition_id": extension.provider_definition_id}


def build_app() -> FastAPI:
    config = LinkConfig(
        service_kind=LinkedServiceKind.VOX,
        peer_name="vox",
        peer_base_url="http://vox.local",
        panel=LinkedServicePanel(title="Vox", path="/ui/", icon=None),
        storage_dir=Path("/var/lib/vox"),
    )
    store: LinkStore[VoxLinkExtension] = LinkStore(
        config.storage_dir,
        extension_from_dict=_ext_from_dict,
        extension_to_dict=_ext_to_dict,
    )
    client: ConduitLinkClient = HttpConduitLinkClient()
    app = FastAPI()
    app.include_router(
        make_link_router(
            config=config,
            store=store,
            client=client,
            build_create_body=_build_create_body,
            build_extension=_build_extension,
            public_response=_public,
        )
    )
    return app
