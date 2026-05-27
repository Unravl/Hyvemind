import React from "react";

interface IconProps {
  size?: number;
  className?: string;
  sw?: number;
  fill?: string;
  stroke?: string;
  children?: React.ReactNode;
}

function Icon({
  size = 24,
  className = "",
  sw = 1.6,
  fill = "none",
  stroke = "currentColor",
  children,
}: IconProps) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={fill}
      stroke={stroke}
      strokeWidth={sw}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      {children}
    </svg>
  );
}

type IconComponent = (props?: Omit<IconProps, "children">) => React.ReactElement;

export const I: Record<string, IconComponent> = {
  dashboard: (p) => (
    <Icon {...p}>
      <rect x="3" y="3" width="8" height="8" rx="1.5" />
      <rect x="13" y="3" width="8" height="4" rx="1.5" />
      <rect x="13" y="9" width="8" height="12" rx="1.5" />
      <rect x="3" y="13" width="8" height="8" rx="1.5" />
    </Icon>
  ),
  dots: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="1.5" fill="currentColor" stroke="none" />
      <circle cx="12" cy="5" r="1.5" fill="currentColor" stroke="none" />
      <circle cx="12" cy="19" r="1.5" fill="currentColor" stroke="none" />
    </Icon>
  ),
  chat: (p) => (
    <Icon {...p}>
      <path d="M21 12a8 8 0 0 1-11.6 7.1L4 21l1.9-5.4A8 8 0 1 1 21 12Z" />
    </Icon>
  ),
  hex: (p) => (
    <Icon {...p}>
      <path d="M12 2 22 7v10l-10 5L2 17V7l10-5Z" />
      <path d="m12 2 0 20" />
      <path d="M2 7l20 10" />
      <path d="M22 7 2 17" />
    </Icon>
  ),
  swarm: (p) => (
    <Icon {...p}>
      <path d="M5 12 9 4l4 4 4-4 4 8-4 8H9Z" />
      <circle cx="12" cy="12" r="2" />
    </Icon>
  ),
  gear: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h0a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51h0a1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v0a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
    </Icon>
  ),
  flask: (p) => (
    <Icon {...p}>
      <path d="M10 2v6L4 18a2 2 0 0 0 1.7 3h12.6A2 2 0 0 0 20 18L14 8V2" />
      <path d="M8.5 13h7" />
      <path d="M9 2h6" />
    </Icon>
  ),
  plus: (p) => (
    <Icon {...p}>
      <path d="M12 5v14M5 12h14" />
    </Icon>
  ),
  minus: (p) => (
    <Icon {...p}>
      <path d="M5 12h14" />
    </Icon>
  ),
  search: (p) => (
    <Icon {...p}>
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </Icon>
  ),
  crosshair: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="8" />
      <path d="M12 2v4" />
      <path d="M12 18v4" />
      <path d="M2 12h4" />
      <path d="M18 12h4" />
    </Icon>
  ),
  send: (p) => (
    <Icon {...p}>
      <path d="M22 2 11 13" />
      <path d="M22 2l-7 20-4-9-9-4 20-7Z" />
    </Icon>
  ),
  rocket: (p) => (
    <Icon {...p}>
      <path d="M4.5 16.5c-1.5 1-2 5-2 5s4-.5 5-2c.6-.9.5-2.2-.3-3-.8-.8-2.1-.9-2.7-.3z" />
      <path d="M12 15 9 12a14 14 0 0 1 9-9c2 0 3 1 3 3a14 14 0 0 1-9 9z" />
      <path d="M9 12H4l3-3a4 4 0 0 1 3-1h2" />
      <path d="m12 15 0 5 3-3a4 4 0 0 0 1-3v-2" />
    </Icon>
  ),
  pause: (p) => (
    <Icon {...p}>
      <rect x="6" y="5" width="4" height="14" rx="1" />
      <rect x="14" y="5" width="4" height="14" rx="1" />
    </Icon>
  ),
  play: (p) => (
    <Icon {...p} fill="currentColor" stroke="none">
      <path d="M6 4l14 8-14 8z" />
    </Icon>
  ),
  skip: (p) => (
    <Icon {...p}>
      <path d="M5 4l10 8-10 8z" />
      <path d="M19 5v14" />
    </Icon>
  ),
  refresh: (p) => (
    <Icon {...p}>
      <path d="M3 12a9 9 0 0 1 15.5-6.4L21 8" />
      <path d="M21 3v5h-5" />
      <path d="M21 12a9 9 0 0 1-15.5 6.4L3 16" />
      <path d="M3 21v-5h5" />
    </Icon>
  ),
  doc: (p) => (
    <Icon {...p}>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <path d="M14 2v6h6" />
      <path d="M8 13h8M8 17h6M8 9h2" />
    </Icon>
  ),
  list: (p) => (
    <Icon {...p}>
      <path d="M8 6h13M8 12h13M8 18h13" />
      <circle cx="3.5" cy="6" r="1" />
      <circle cx="3.5" cy="12" r="1" />
      <circle cx="3.5" cy="18" r="1" />
    </Icon>
  ),
  check: (p) => (
    <Icon {...p}>
      <path d="M5 12l5 5 9-11" />
    </Icon>
  ),
  x: (p) => (
    <Icon {...p}>
      <path d="M6 6l12 12M18 6 6 18" />
    </Icon>
  ),
  trash: (p) => (
    <Icon {...p}>
      <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M6 6l1 14a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-14" />
      <path d="M10 11v6M14 11v6" />
    </Icon>
  ),
  copy: (p) => (
    <Icon {...p}>
      <rect x="8" y="8" width="13" height="13" rx="2" />
      <path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" />
    </Icon>
  ),
  edit: (p) => (
    <Icon {...p}>
      <path d="M12 20h9" />
      <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" />
    </Icon>
  ),
  beaker: (p) => (
    <Icon {...p}>
      <path d="M9 3h6" />
      <path d="M10 3v8L4 21h16L14 11V3" />
    </Icon>
  ),
  arrow: (p) => (
    <Icon {...p}>
      <path d="M5 12h14M13 5l7 7-7 7" />
    </Icon>
  ),
  folder: (p) => (
    <Icon {...p}>
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    </Icon>
  ),
  chevD: (p) => (
    <Icon {...p}>
      <path d="m6 9 6 6 6-6" />
    </Icon>
  ),
  chevR: (p) => (
    <Icon {...p}>
      <path d="m9 6 6 6-6 6" />
    </Icon>
  ),
  chevL: (p) => (
    <Icon {...p}>
      <path d="m15 6-6 6 6 6" />
    </Icon>
  ),
  cost: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M14 9.5a3 3 0 0 0-2.5-1.5h-1A2 2 0 0 0 8.5 10v0a2 2 0 0 0 1.4 1.9l3.2 1.2a2 2 0 0 1 1.4 1.9v0a2 2 0 0 1-2 2h-1A3 3 0 0 1 9 15.5" />
      <path d="M12 6v2M12 16v2" />
    </Icon>
  ),
  clock: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </Icon>
  ),
  bug: (p) => (
    <Icon {...p}>
      <path d="M8 6V5a4 4 0 1 1 8 0v1" />
      <rect x="6" y="6" width="12" height="13" rx="6" />
      <path d="M3 12h3M18 12h3M3 6l3 2M21 6l-3 2M3 18l3-2M21 18l-3-2M12 6v13" />
    </Icon>
  ),
  spark: (p) => (
    <Icon {...p}>
      <path d="M12 3v4M12 17v4M3 12h4M17 12h4M5.6 5.6l2.8 2.8M15.6 15.6l2.8 2.8M5.6 18.4l2.8-2.8M15.6 8.4l2.8-2.8" />
    </Icon>
  ),
  brain: (p) => (
    <Icon {...p}>
      <path d="M9 4a3 3 0 0 0-3 3v0a3 3 0 0 0-3 3v1a3 3 0 0 0 1.5 2.6A3 3 0 0 0 6 18a3 3 0 0 0 6 0V7a3 3 0 0 0-3-3Z" />
      <path d="M15 4a3 3 0 0 1 3 3v0a3 3 0 0 1 3 3v1a3 3 0 0 1-1.5 2.6A3 3 0 0 1 18 18a3 3 0 0 1-6 0" />
    </Icon>
  ),
  shield: (p) => (
    <Icon {...p}>
      <path d="M12 3 4 6v6c0 5 3.5 8.5 8 9 4.5-.5 8-4 8-9V6Z" />
    </Icon>
  ),
  cross: (p) => (
    <Icon {...p}>
      <path d="M9 3h6v6h6v6h-6v6H9v-6H3V9h6Z" />
    </Icon>
  ),
  crown: (p) => (
    <Icon {...p}>
      <path d="m3 18 2-10 5 5 2-7 2 7 5-5 2 10z" />
      <path d="M3 18h18" />
    </Icon>
  ),
  scope: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 3v4M12 17v4M3 12h4M17 12h4" />
      <circle cx="12" cy="12" r="2" />
    </Icon>
  ),
  hexFill: (p) => (
    <Icon {...p}>
      <path
        d="M12 2 22 7v10l-10 5L2 17V7Z"
        fill="currentColor"
        stroke="none"
      />
    </Icon>
  ),
  info: (p) => (
    <Icon {...p}>
      <circle cx="12" cy="12" r="10" />
      <path d="M12 16v-4" />
      <path d="M12 8h.01" />
    </Icon>
  ),
  terminal: (p) => (
    <Icon {...p}>
      <rect x="2" y="3" width="20" height="18" rx="3" />
      <path d="M7 8l4 4-4 4" />
      <path d="M13 16h4" />
    </Icon>
  ),
  grip: (p) => (
    <Icon {...p}>
      <circle cx="9" cy="6" r="1" fill="currentColor" stroke="none" />
      <circle cx="15" cy="6" r="1" fill="currentColor" stroke="none" />
      <circle cx="9" cy="12" r="1" fill="currentColor" stroke="none" />
      <circle cx="15" cy="12" r="1" fill="currentColor" stroke="none" />
      <circle cx="9" cy="18" r="1" fill="currentColor" stroke="none" />
      <circle cx="15" cy="18" r="1" fill="currentColor" stroke="none" />
    </Icon>
  ),
  tag: (p) => (
    <Icon {...p}>
      <path d="M12 2H2v10l9.2 9.2a1 1 0 0 0 1.4 0l6.6-6.6a1 1 0 0 0 0-1.4L12 2Z" />
      <circle cx="7" cy="7" r="1.5" />
    </Icon>
  ),
  scissors: (p) => (
    <Icon {...p}>
      <circle cx="6" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M20 4 8.12 15.88" />
      <path d="M14.47 14.48 20 20" />
      <path d="M8.12 8.12 12 12" />
    </Icon>
  ),
  paste: (p) => (
    <Icon {...p}>
      <rect x="8" y="8" width="13" height="13" rx="2" />
      <path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" />
      <path d="M12 11v5" />
      <path d="M9.5 13.5h5" />
    </Icon>
  ),
  selectAll: (p) => (
    <Icon {...p}>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <path d="M8 12h8" />
      <path d="M8 8h8" />
      <path d="M8 16h8" />
    </Icon>
  ),
  filter: (p) => (
    <Icon {...p}>
      <path d="M3 6h18M6 12h12M9 18h6" />
    </Icon>
  ),
  sort: (p) => (
    <Icon {...p}>
      <path d="M7 15l5 5 5-5" />
      <path d="M7 9l5-5 5 5" />
    </Icon>
  ),
  heart: (p) => (
    <Icon {...p}>
      <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
    </Icon>
  ),
};
