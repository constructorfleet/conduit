import { describe, expect, it } from "vitest";

import { concatSamples, encodeWav } from "./recorder";

/// Reads the file back the way the server's parser does.
async function header(blob: Blob) {
  const view = new DataView(await blob.arrayBuffer());
  const ascii = (at: number, length: number) =>
    Array.from({ length }, (_, index) =>
      String.fromCharCode(view.getUint8(at + index)),
    ).join("");
  return {
    riff: ascii(0, 4),
    wave: ascii(8, 4),
    data: ascii(36, 4),
    declaredLength: view.getUint32(4, true),
    formatCode: view.getUint16(20, true),
    channels: view.getUint16(22, true),
    sampleRate: view.getUint32(24, true),
    byteRate: view.getUint32(28, true),
    blockAlign: view.getUint16(32, true),
    bits: view.getUint16(34, true),
    dataLength: view.getUint32(40, true),
    sampleAt: (index: number) => view.getInt16(44 + index * 2, true),
  };
}

describe("recording a voice as a WAV file", () => {
  it("writes a header the server can read", async () => {
    const blob = encodeWav(new Float32Array(1600), 44_100);
    const wav = await header(blob);

    expect(blob.type).toBe("audio/wav");
    expect(wav.riff).toBe("RIFF");
    expect(wav.wave).toBe("WAVE");
    expect(wav.data).toBe("data");
    expect(wav.formatCode).toBe(1);
    expect(wav.channels).toBe(1);
    expect(wav.bits).toBe(16);
    expect(wav.blockAlign).toBe(2);
  });

  it("records the rate the microphone actually ran at", async () => {
    // The whole reason the file is a WAV: a browser captures at whatever its
    // hardware runs at, and audio sent at the wrong rate is a voice pitched
    // wrong, which embeds as a different person.
    const wav = await header(encodeWav(new Float32Array(8), 48_000));

    expect(wav.sampleRate).toBe(48_000);
    expect(wav.byteRate).toBe(96_000);
  });

  it("declares sizes that match the samples it carries", async () => {
    // A length that disagrees with the payload is the classic way to make a
    // file every player opens and every decoder truncates.
    const blob = encodeWav(new Float32Array(100), 16_000);
    const wav = await header(blob);

    expect(blob.size).toBe(44 + 200);
    expect(wav.dataLength).toBe(200);
    expect(wav.declaredLength).toBe(36 + 200);
  });

  it("clamps a microphone that clipped rather than wrapping it", async () => {
    const wav = await header(
      encodeWav(Float32Array.from([0, 0.5, -0.5, 2, -2]), 16_000),
    );

    expect(wav.sampleAt(0)).toBe(0);
    expect(wav.sampleAt(1)).toBe(16384);
    // Rounding takes halves upward, so the negative half lands one nearer
    // zero than its positive twin. Inaudible, and worth stating rather than
    // rediscovering.
    expect(wav.sampleAt(2)).toBe(-16383);
    expect(wav.sampleAt(3)).toBe(32767);
    expect(wav.sampleAt(4)).toBe(-32767);
  });

  it("joins the captured blocks in the order they arrived", () => {
    const joined = concatSamples([
      Float32Array.from([1, 2]),
      Float32Array.from([3]),
      Float32Array.from([4, 5]),
    ]);

    expect(Array.from(joined)).toEqual([1, 2, 3, 4, 5]);
  });

  it("joins nothing into nothing", () => {
    // Stopping a recording that captured no callback yet must produce an
    // empty file rather than throw; the server refuses it with a message the
    // operator can act on.
    expect(concatSamples([]).length).toBe(0);
  });
});
