import { useEffect, useState } from "preact/hooks";

/** Re-render whenever a store announces a change. */
export function useSubscribe(subscribe: (fn: () => void) => () => void) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const bump = () => setTick((t) => t + 1);
    const unsubscribe = subscribe(bump);
    // Changes between the first render and now went unheard (the daemon's
    // hello can beat this effect); catch up once.
    bump();
    return unsubscribe;
  }, []);
}

/** True below the phone breakpoint, tracking window resizes. */
export function usePhone(): boolean {
  const query = "(max-width: 700px)";
  const [phone, setPhone] = useState(() => matchMedia(query).matches);
  useEffect(() => {
    const m = matchMedia(query);
    const fn = () => setPhone(m.matches);
    m.addEventListener("change", fn);
    return () => m.removeEventListener("change", fn);
  }, []);
  return phone;
}
