# Conduit's Wyoming speech recognition service.
#
# One file builds both published tags, because two Dockerfiles for one service
# drift and only one of them gets tested:
#
#   CPU: docker build -t conduit-wyoming-asr:canary services/wyoming-asr
#   GPU: docker build -t conduit-wyoming-asr:canary-gpu \
#          --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
#          --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu126 \
#          --build-arg DEVICE=cuda services/wyoming-asr
ARG BASE_IMAGE=python:3.12-slim-bookworm
FROM ${BASE_IMAGE}

# Repeated after FROM: an ARG declared before it is out of scope afterwards.
ARG TORCH_INDEX=https://download.pytorch.org/whl/cpu
ARG DEVICE=cpu
# Which recognizer to install. A transformers-based engine would point this at
# its own file and never pay for NeMo, which is the largest thing in the image.
ARG ENGINE_REQUIREMENTS=requirements-engine.txt

# The CUDA images ship no Python, and NeMo's audio stack needs libsndfile.
# Installing both unconditionally keeps one path through this file.
RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        libsndfile1 \
        python3 \
        python3-pip \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

# A virtualenv rather than --break-system-packages: Debian's Python is managed
# by apt, and pip writing into it is a conflict waiting for the next apt upgrade.
ENV VIRTUAL_ENV=/opt/venv
RUN python3 -m venv "${VIRTUAL_ENV}"
ENV PATH="${VIRTUAL_ENV}/bin:${PATH}"

WORKDIR /srv

# Torch first and from its own index, so the CPU image pulls CPU wheels rather
# than the multi-gigabyte CUDA build that PyPI serves by default. This layer is
# the expensive one and changes only when the pin does.
COPY requirements-torch.txt ./
RUN pip install --no-cache-dir --index-url "${TORCH_INDEX}" -r requirements-torch.txt

# The recognizer next, in its own layer for the same reason: NeMo changes far
# less often than app.py does.
COPY requirements-engine.txt ./
RUN pip install --no-cache-dir -r "${ENGINE_REQUIREMENTS}"

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY app.py ./

# Written to at runtime, so it exists and is owned before the drop to a non-root
# user. It is a mount point in the compose file.
RUN useradd --create-home --shell /usr/sbin/nologin asr \
    && mkdir -p /models \
    && chown asr:asr /models

ENV ASR_ENGINE=canary \
    ASR_MODEL_DIR=/models \
    ASR_DEVICE=${DEVICE} \
    ASR_URI=tcp://0.0.0.0:10300 \
    # Where NeMo and HuggingFace put their own caches. Pointed at the model
    # volume so a restart does not re-download several gigabytes into a layer
    # that vanishes with the container.
    HF_HOME=/models/huggingface \
    NEMO_CACHE_DIR=/models/nemo

USER asr
EXPOSE 10300
ENTRYPOINT ["python", "app.py"]
