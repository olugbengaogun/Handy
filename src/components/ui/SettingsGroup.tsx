import React from "react";

interface SettingsGroupProps {
  title?: string;
  description?: string;
  children: React.ReactNode;
}

export const SettingsGroup: React.FC<SettingsGroupProps> = ({
  title,
  description,
  children,
}) => {
  return (
    // Spacing, radius and rule weight live here rather than in each screen, so
    // one edit re-tunes every settings page at once — and, per CLAUDE.md, so a
    // visual pass never has to touch the many files upstream also edits.
    <div className="space-y-2.5">
      {title && (
        <div className="px-4">
          <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wider">
            {title}
          </h2>
          {description && (
            <p className="text-xs text-mid-gray/90 mt-1 leading-relaxed max-w-prose">
              {description}
            </p>
          )}
        </div>
      )}
      {/* A larger radius and a lighter internal rule: the hairline between rows
          only needs to separate them, and at /20 it was competing with the
          card's own edge for attention. */}
      <div className="bg-surface border border-mid-gray/20 rounded-xl shadow-sm overflow-visible">
        <div className="divide-y divide-mid-gray/15">{children}</div>
      </div>
    </div>
  );
};
