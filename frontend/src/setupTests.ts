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

afterEach(() => {
  cleanup();
});
