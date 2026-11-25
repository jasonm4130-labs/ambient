import { useCallback, useRef } from "react";

/// Drops a reply whose call was superseded, so a late reply never clobbers a newer one.
export function useLatest<Args extends unknown[], T>(
  call: (...args: Args) => Promise<T>,
): (...args: Args) => Promise<T | undefined> {
  const generation = useRef(0);

  return useCallback(
    (...args: Args) => {
      generation.current += 1;
      const issued = generation.current;
      return call(...args).then(
        (result): T | undefined => (generation.current === issued ? result : undefined),
        (error: unknown): undefined => {
          if (generation.current === issued) throw error;
          return undefined;
        },
      );
    },
    [call],
  );
}
