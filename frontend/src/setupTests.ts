import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom implements no layout, so it ships no `scrollIntoView` at all. The event
// story scrolls a row into view inside a `requestAnimationFrame` callback, which
// is outside any test's call stack: the `TypeError` does not fail the test that
// jumped to a row, it escapes as an unhandled error after every test has already
// passed, so the suite reports 158 passed and 1 error and exits non-zero. Whether
// jsdom flushes the callback before the run ends is a race, which is why this
// only ever failed on CI. A no-op is the whole behaviour worth asserting here —
// nothing can observe scrolling in a jsdom document.
Element.prototype.scrollIntoView = function scrollIntoView() {};

// Node 26 ships a built-in Web Storage `localStorage`, and under vitest 4 it
// shadowed jsdom's: `localStorage` came out `undefined` inside tests and 109 of
// them failed on `localStorage.clear()` with nothing naming the cause (#285).
// vitest 5 resolves it, so this is a tripwire rather than a fix — if a Node or
// runner change puts the wrong storage back, the suite says which one sentence
// instead of failing a hundred tests on a missing method.
if (
  typeof localStorage === "undefined" ||
  localStorage !== window.localStorage
) {
  throw new Error(
    "the test environment's localStorage is not jsdom's — a built-in Web Storage " +
      "global is shadowing it (see #285); check the Node and vitest versions",
  );
}

afterEach(() => {
  cleanup();
});
