import { useEffect, useRef } from 'react';
import { Sparkles, X } from 'lucide-react';

/** 原生模态框提供焦点约束与 Escape 关闭，避免键盘操作穿透到背景。 */
export function InfoDialog({ message, onClose }: { message: string; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  return (
    <dialog ref={dialog} className="info-modal" aria-labelledby="info-title" onCancel={onClose}>
      <button className="icon-button modal-close" aria-label="关闭" onClick={onClose} autoFocus>
        <X size={18} />
      </button>
      <Sparkles size={26} />
      <h2 id="info-title">正在准备中</h2>
      <p>{message}</p>
      <button className="secondary-button" onClick={onClose}>
        知道了
      </button>
    </dialog>
  );
}
