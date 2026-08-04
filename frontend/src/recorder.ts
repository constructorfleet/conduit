/// Recording a voice in the page, as a file the server can read.
///
/// `MediaRecorder` is the obvious tool and the wrong one: it produces WebM or
/// MP4, and the server would need a decoder for whichever the browser picked.
/// So capture goes through Web Audio instead, which hands over raw float
/// samples, and those are written into a WAV header here. A WAV says its own
/// sample rate, so audio captured at whatever the microphone runs at arrives
/// correctly rather than at the wrong speed.

/// Bytes per sample in the file this writes.
const BYTES_PER_SAMPLE = 2;

/// How many frames each capture callback delivers.
///
/// A quarter of a second at 48 kHz. Large enough that the callback is not the
/// bottleneck, small enough that stopping feels immediate.
const CAPTURE_FRAMES = 4096;

/// Wraps float samples in a WAV header, as mono 16-bit PCM at `sampleRate`.
///
/// Exported for its own sake: this is the part with arithmetic in it, and it
/// is testable without a microphone.
export function encodeWav(samples: Float32Array, sampleRate: number): Blob {
  const bytes = new ArrayBuffer(44 + samples.length * BYTES_PER_SAMPLE);
  const view = new DataView(bytes);

  const ascii = (at: number, text: string) => {
    for (let index = 0; index < text.length; index += 1) {
      view.setUint8(at + index, text.charCodeAt(index));
    }
  };

  const dataLength = samples.length * BYTES_PER_SAMPLE;
  ascii(0, "RIFF");
  view.setUint32(4, 36 + dataLength, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // integer PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * BYTES_PER_SAMPLE, true); // byte rate
  view.setUint16(32, BYTES_PER_SAMPLE, true); // block align
  view.setUint16(34, 8 * BYTES_PER_SAMPLE, true);
  ascii(36, "data");
  view.setUint32(40, dataLength, true);

  for (let index = 0; index < samples.length; index += 1) {
    // Clamped rather than wrapped: a microphone that clips slightly is
    // ordinary, and wrapping turns the loudest moment into its opposite.
    const sample = Math.max(-1, Math.min(1, samples[index]));
    view.setInt16(
      44 + index * BYTES_PER_SAMPLE,
      Math.round(sample * 32767),
      true,
    );
  }

  return new Blob([bytes], { type: "audio/wav" });
}

/// Joins the captured blocks into one run of samples.
export function concatSamples(blocks: readonly Float32Array[]): Float32Array {
  const total = blocks.reduce((sum, block) => sum + block.length, 0);
  const joined = new Float32Array(total);
  let at = 0;
  for (const block of blocks) {
    joined.set(block, at);
    at += block.length;
  }
  return joined;
}

/// A recording in progress.
export interface Recording {
  /// Stops capture and returns what was recorded as a WAV file.
  stop: () => Promise<Blob>;
  /// Abandons the recording, releasing the microphone without producing a
  /// file. What an operator who changed their mind gets.
  cancel: () => void;
}

/// Starts recording from the default microphone.
///
/// The caller is responsible for stopping or cancelling: until one of those
/// happens the browser shows the tab as recording, which is exactly the
/// feedback somebody being recorded should have.
///
/// Rejects if permission is refused or the browser has no microphone API,
/// which is the same thing as far as the page is concerned: there is nothing
/// to record, and the operator should upload a file instead.
export async function startRecording(): Promise<Recording> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error(
      "This browser will not give the page a microphone; upload a WAV file instead",
    );
  }

  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const context = new AudioContext();
  const source = context.createMediaStreamSource(stream);
  // Deprecated in favour of an AudioWorklet, which needs a separate module
  // file the bundler has to emit; this stays until that is worth doing, and
  // it is the one path that works in every browser today.
  const processor = context.createScriptProcessor(CAPTURE_FRAMES, 1, 1);
  const blocks: Float32Array[] = [];

  processor.onaudioprocess = (event) => {
    // Copied: the buffer is reused by the next callback, so keeping the
    // reference would leave every block holding the same final audio.
    blocks.push(new Float32Array(event.inputBuffer.getChannelData(0)));
  };

  source.connect(processor);
  // Connected to the destination because a processor with no consumer is not
  // pulled in some browsers, and never fires. The gain is zero, so nothing is
  // played back — a live speaker would be a feedback loop.
  const silence = context.createGain();
  silence.gain.value = 0;
  processor.connect(silence);
  silence.connect(context.destination);

  const release = () => {
    processor.onaudioprocess = null;
    processor.disconnect();
    silence.disconnect();
    source.disconnect();
    stream.getTracks().forEach((track) => track.stop());
    void context.close();
  };

  return {
    stop: async () => {
      const sampleRate = context.sampleRate;
      release();
      return encodeWav(concatSamples(blocks), sampleRate);
    },
    cancel: () => {
      blocks.length = 0;
      release();
    },
  };
}
