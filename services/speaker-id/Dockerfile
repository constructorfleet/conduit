# Conduit's speaker identification service.
#
# One file builds both published tags, because two Dockerfiles for one service
# drift and only one of them gets tested:
#
#   CPU: docker build -t conduit-speaker-id:speechbrain services/speaker-id
#   GPU: docker build -t conduit-speaker-id:speechbrain-gpu \
#          --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
#          --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu126 \
#          --build-arg DEVICE=cuda services/speaker-id
ARG BASE_IMAGE=python:3.12-slim-bookworm
FROM ${BASE_IMAGE}

# Repeated after FROM: an ARG declared before it is out of scope afterwards.
ARG TORCH_INDEX=https://download.pytorch.org/whl/cpu
ARG DEVICE=cpu

# The CUDA images ship no Python, and the slim Python image ships no libsndfile.
# Installing both unconditionally keeps one path through this file.
RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        libsndfile1 \
        python3 \
        python3-pip \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

# A virtualenv rather than --break-system-packages: Debian's Python is managed
# by apt, and pip writing into it is a conflict waiting for the next apt
# upgrade.
ENV VIRTUAL_ENV=/opt/venv
RUN python3 -m venv "${VIRTUAL_ENV}"
ENV PATH="${VIRTUAL_ENV}/bin:${PATH}"

WORKDIR /srv

# Torch first and from its own index, so the CPU image pulls CPU wheels rather
# than the multi-gigabyte CUDA build that PyPI serves by default. This layer is
# the expensive one and changes only when the pin does.
COPY requirements-torch.txt ./
RUN pip install --no-cache-dir --index-url "${TORCH_INDEX}" -r requirements-torch.txt

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY app.py ./

# Written to at runtime, so they exist and are owned before the drop to a
# non-root user. Both are mount points in the compose file.
RUN useradd --create-home --shell /usr/sbin/nologin speaker \
    && mkdir -p /data /models \
    && chown speaker:speaker /data /models

ENV SPEAKER_ID_ENGINE=speechbrain \
    SPEAKER_ID_DATA_DIR=/data \
    SPEAKER_ID_MODEL_DIR=/models \
    SPEAKER_ID_DEVICE=${DEVICE} \
    # Where SpeechBrain and HuggingFace put their own caches. Pointed at the
    # model volume so a restart does not re-download the model into a layer
    # that vanishes with the container.
    HF_HOME=/models/huggingface

USER speaker
EXPOSE 8080
ENTRYPOINT ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
