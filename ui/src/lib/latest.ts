import { useCallback, useRef } from "react";

/// Wraps a bridge call with a generation counter: each call bumps it, and a
/// reply is applied only if the generation it was issued under is still the
/// current one. Two calls issued A then B whose replies arrive B then A leave
/// B's result in place — A's reply, arriving late, is dropped rather than
/// clobbering what B already rendered.
export function useLatest<Args extends unknown[], T>(
  call: (...args: Args) => Promise<T>,
): (...args: Args) => Promise<T | undefined> {
  const generation = useRef(0);

  return useCallback(
    (...args: Args) => {
      generation.current += 1;
      const issued = generation.current;
      return call(...args).then((result): T | undefined =>
        generation.current === issued ? result : undefined,
      );
    },
    [call],
  );
}
