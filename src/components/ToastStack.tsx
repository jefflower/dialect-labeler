import { AlertCircle, CheckCircle2, Info, TriangleAlert, X } from "lucide-react";
import type { Toast } from "../types";

const variantIcon = {
  info: Info,
  success: CheckCircle2,
  warning: TriangleAlert,
  error: AlertCircle,
};

type ToastStackProps = {
  toasts: Toast[];
  onDismiss: (id: number) => void;
};

export function ToastStack({ toasts, onDismiss }: ToastStackProps) {
  if (toasts.length === 0) return null;
  return (
    <div className="toast-stack">
      {toasts.map((toast) => {
        const Icon = variantIcon[toast.variant];
        return (
          <div className={`toast ${toast.variant}`} key={toast.id}>
            <span className="icon">
              <Icon size={16} />
            </span>
            <div className="copy">
              <strong>{toast.title}</strong>
              {toast.detail && <span>{toast.detail}</span>}
            </div>
            <button
              className="btn-ghost btn-icon close"
              onClick={() => onDismiss(toast.id)}
              aria-label="关闭通知"
            >
              <X size={14} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
