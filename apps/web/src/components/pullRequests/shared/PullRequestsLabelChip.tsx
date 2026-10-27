import type { PullRequestsLabel } from "@coddy/contracts";
function labelColors(value: string | null) {
  if (!value || !/^#?(?:[\da-f]{3}|[\da-f]{6})$/i.test(value)) return undefined;
  let hex = value.replace(/^#/, "");
  if (hex.length === 3) hex = [...hex].map((c) => c + c).join("");
  const rgb = [0, 2, 4].map((offset) => {
    const channel = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  const luminance = rgb[0]! * 0.2126 + rgb[1]! * 0.7152 + rgb[2]! * 0.0722;
  return { backgroundColor: `#${hex}`, color: luminance > 0.179 ? "#000000" : "#ffffff" };
}
export function PullRequestsLabelChip({ label }: { label: PullRequestsLabel }) {
  return (
    <span
      className="inline-block max-w-32 shrink-0 truncate rounded-full border border-border/50 bg-muted px-1.5 py-px text-xs font-medium"
      style={labelColors(label.color)}
      title={label.description ?? label.name}
    >
      {label.name}
    </span>
  );
}
