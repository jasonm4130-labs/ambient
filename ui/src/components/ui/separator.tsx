import { cn } from "@/lib/utils";

export interface SeparatorProps {
  className?: string;
}

export function Separator({ className }: SeparatorProps) {
  return (
    <div
      data-slot="separator"
      role="separator"
      aria-orientation="horizontal"
      className={cn("bg-border h-px w-full shrink-0", className)}
    />
  );
}
