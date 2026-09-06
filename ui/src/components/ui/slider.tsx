import type { InputHTMLAttributes } from "react";
import { cn } from "@/lib/utils";

export interface SliderProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {}

// A real `<input type="range">`, not a Radix slider: same reasoning as
// `select.tsx` — real accessible-role and real hardware for `uicheck`.
export function Slider({ className, ...props }: SliderProps) {
  return (
    <input
      type="range"
      data-slot="slider"
      className={cn("accent-primary h-2 w-full disabled:cursor-not-allowed disabled:opacity-50", className)}
      {...props}
    />
  );
}
