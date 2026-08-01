const STORAGE_KEY = "conduit.operator.access";

export type OperatorAccess =
  | { mode: "none" }
  | { mode: "bearer"; token: string; persisted: boolean }
  | { mode: "anonymous"; persisted: false };

type StoredAccess = { mode: "bearer"; token: string } | { mode: "anonymous" };

export function loadOperatorAccess(): OperatorAccess {
  const session = readAccess(sessionStorage.getItem(STORAGE_KEY));
  if (session) {
    return { ...session, persisted: false };
  }

  const local = readAccess(localStorage.getItem(STORAGE_KEY));
  if (!local) {
    return { mode: "none" };
  }
  return local.mode === "bearer"
    ? { ...local, persisted: true }
    : { ...local, persisted: false };
}

export function saveBearerAccess(
  token: string,
  remember: boolean,
): OperatorAccess {
  const trimmed = token.trim();
  if (!trimmed) {
    throw new Error("management bearer token is required");
  }

  const stored: StoredAccess = { mode: "bearer", token: trimmed };
  if (remember) {
    sessionStorage.removeItem(STORAGE_KEY);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
    return { ...stored, persisted: true };
  }

  localStorage.removeItem(STORAGE_KEY);
  sessionStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
  return { ...stored, persisted: false };
}

export function saveAnonymousAccess(): OperatorAccess {
  const stored: StoredAccess = { mode: "anonymous" };
  localStorage.removeItem(STORAGE_KEY);
  sessionStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
  return { mode: "anonymous", persisted: false };
}

export function clearOperatorAccess(): void {
  sessionStorage.removeItem(STORAGE_KEY);
  localStorage.removeItem(STORAGE_KEY);
}

function readAccess(value: string | null): StoredAccess | null {
  if (!value) {
    return null;
  }

  try {
    const parsed = JSON.parse(value) as Partial<StoredAccess>;
    if (parsed.mode === "anonymous") {
      return { mode: "anonymous" };
    }
    if (parsed.mode === "bearer" && typeof parsed.token === "string") {
      return { mode: "bearer", token: parsed.token };
    }
  } catch {
    return null;
  }
  return null;
}
