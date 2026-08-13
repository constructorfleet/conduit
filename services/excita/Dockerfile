FROM python:3.12-slim

WORKDIR /app

COPY services/excita/requirements.txt ./requirements.txt
COPY packages/conduit-link ./conduit-link
RUN pip install --no-cache-dir -r requirements.txt \
 && pip install --no-cache-dir ./conduit-link

COPY services/excita ./excita

ENV EXCITA_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8084

CMD ["python", "-m", "excita.app"]
