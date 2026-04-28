import type { ReactNode } from "react";

type EmptyStateProps = {
  icon: ReactNode;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
};

export function EmptyState({ icon, title, description, action }: EmptyStateProps) {
  return (
    <div className="empty-state">
      <div className="empty-state-inner">
        <div className="empty-state-icon">{icon}</div>
        <strong>{title}</strong>
        {description && <span>{description}</span>}
        {action}
      </div>
    </div>
  );
}
