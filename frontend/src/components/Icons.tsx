/** The mockups' icon set — Feather-style 24x24 strokes drawn in
 *  `currentColor` at 1.7-1.8 weight, so a parent's `color` drives them.
 *  Kept as one file because they're design assets, not logic. */

type P = { size?: number; strokeWidth?: number };

function Svg({ size = 14, strokeWidth = 1.7, children }: P & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export const IconStack = (p: P) => (
  <Svg {...p}>
    <rect x="2" y="3" width="20" height="8" rx="2" />
    <rect x="2" y="13" width="20" height="8" rx="2" />
    <line x1="6" y1="7" x2="6.01" y2="7" />
    <line x1="6" y1="17" x2="6.01" y2="17" />
  </Svg>
);

export const IconFolder = (p: P) => (
  <Svg {...p}>
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
  </Svg>
);

export const IconDatabase = (p: P) => (
  <Svg {...p}>
    <ellipse cx="12" cy="5" rx="9" ry="3" />
    <path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3" />
    <path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5" />
  </Svg>
);

export const IconCode = (p: P) => (
  <Svg {...p}>
    <polyline points="16 18 22 12 16 6" />
    <polyline points="8 6 2 12 8 18" />
  </Svg>
);

export const IconGear = (p: P) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6h.09A1.65 1.65 0 0 0 10.6 3.09V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
  </Svg>
);

export const IconCamera = (p: P) => (
  <Svg {...p}>
    <path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2Z" />
    <circle cx="12" cy="13" r="3.4" />
  </Svg>
);

export const IconPlus = ({ size = 13, strokeWidth = 2.4 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </Svg>
);

export const IconChevronLeft = ({ size = 14, strokeWidth = 2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <polyline points="15 18 9 12 15 6" />
  </Svg>
);

export const IconChevronRight = ({ size = 14, strokeWidth = 2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <polyline points="9 18 15 12 9 6" />
  </Svg>
);

export const IconChevronDown = ({ size = 13, strokeWidth = 2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <polyline points="6 9 12 15 18 9" />
  </Svg>
);

export const IconDots = ({ size = 14, strokeWidth = 2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <circle cx="12" cy="12" r="1.6" />
    <circle cx="19" cy="12" r="1.6" />
    <circle cx="5" cy="12" r="1.6" />
  </Svg>
);

export const IconCheck = ({ size = 16, strokeWidth = 2.2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <polyline points="20 6 9 17 4 12" />
  </Svg>
);

export const IconInfo = ({ size = 15, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <circle cx="12" cy="12" r="9" />
    <line x1="12" y1="16" x2="12" y2="12" />
    <line x1="12" y1="8" x2="12.01" y2="8" />
  </Svg>
);

export const IconWarn = ({ size = 15, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" />
    <line x1="12" y1="9" x2="12" y2="13" />
    <line x1="12" y1="17" x2="12.01" y2="17" />
  </Svg>
);

export const IconShield = ({ size = 17, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10Z" />
  </Svg>
);

export const IconExternal = ({ size = 14, strokeWidth = 2 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
    <polyline points="15 3 21 3 21 9" />
    <line x1="10" y1="14" x2="21" y2="3" />
  </Svg>
);

export const IconCopy = ({ size = 13, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <rect x="9" y="9" width="12" height="12" rx="2" />
    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
  </Svg>
);

export const IconTerminal = ({ size = 14, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <polyline points="4 17 10 11 4 5" />
    <line x1="12" y1="19" x2="20" y2="19" />
  </Svg>
);

export const IconBug = ({ size = 14, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <rect x="8" y="6" width="8" height="14" rx="4" />
    <path d="M19 8h-3M19 13h-3M19 18h-3M5 8h3M5 13h3M5 18h3M9 6a3 3 0 0 1 6 0" />
  </Svg>
);

export const IconGrid = (p: P) => (
  <Svg {...p}>
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </Svg>
);

export const IconActivity = (p: P) => (
  <Svg {...p}>
    <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
  </Svg>
);

export const IconBranch = ({ size = 12, strokeWidth = 1.9 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <circle cx="6" cy="5" r="2.2" />
    <circle cx="6" cy="19" r="2.2" />
    <circle cx="18" cy="9" r="2.2" />
    <path d="M6 7.2v9.6M8.2 5.6h5.6a4 4 0 0 1 4 4v0" />
  </Svg>
);

export const IconChart = (p: P) => (
  <Svg {...p}>
    <line x1="3" y1="21" x2="21" y2="21" />
    <rect x="5" y="12" width="4" height="6" rx="1" />
    <rect x="11" y="7" width="4" height="11" rx="1" />
    <rect x="17" y="14" width="4" height="4" rx="1" />
  </Svg>
);

export const IconTrash = ({ size = 13, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14" />
  </Svg>
);

export const IconUndo = ({ size = 13, strokeWidth = 1.9 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M3 10h11a5 5 0 0 1 0 10h-3" />
    <polyline points="7 6 3 10 7 14" />
  </Svg>
);

export const IconSun = ({ size = 14, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </Svg>
);

export const IconMoon = ({ size = 14, strokeWidth = 1.8 }: P) => (
  <Svg size={size} strokeWidth={strokeWidth}>
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" />
  </Svg>
);

export const Logo = ({ size = 18 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true" style={{ flexShrink: 0 }}>
    <circle cx="6" cy="7" r="2.4" fill="var(--acc)" />
    <circle cx="18" cy="7" r="2.4" fill="var(--acc)" opacity="0.45" />
    <circle cx="12" cy="18" r="2.4" fill="var(--acc)" opacity="0.75" />
    <path
      d="M8.4 7h7.2M7.4 9.2 11 15.8M16.6 9.2 13 15.8"
      stroke="var(--acc)"
      strokeWidth="1.3"
      strokeLinecap="round"
      opacity="0.4"
    />
  </svg>
);
