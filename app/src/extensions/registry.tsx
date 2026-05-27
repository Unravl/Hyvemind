import React, { type ComponentType, useState } from "react";
import {
  DndContext,
  PointerSensor,
  KeyboardSensor,
  useSensor,
  useSensors,
  closestCenter,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  useSortable,
  arrayMove,
  horizontalListSortingStrategy,
  sortableKeyboardCoordinates,
} from "@dnd-kit/sortable";
import {
  restrictToHorizontalAxis,
  restrictToParentElement,
} from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import type { SnapshotEntry } from "./types";
import { DefaultUsagePill } from "./widgets/DefaultUsagePill";
import { ChatGptSubUsagePill } from "./widgets/ChatGptSubUsagePill";
import { ClaudeSubUsagePill } from "./widgets/ClaudeSubUsagePill";
import { CrofUsagePill } from "./widgets/CrofUsagePill";
import { DeepseekBalancePill } from "./widgets/DeepseekBalancePill";
import { NeokensBalancePill } from "./widgets/NeokensBalancePill";
import { NeuralWattUsagePill } from "./widgets/NeuralWattUsagePill";
import { OpenRouterCreditsPill } from "./widgets/OpenRouterCreditsPill";
import { useExtensions } from "./useExtensions";
import { loadOrder, saveOrder, applyOrder } from "./topbarOrder";

export type ExtensionWidget = ComponentType<{ entry: SnapshotEntry }>;

/** Module-level singleton — populated by `registerWidgets()` at app
 *  init. Keyed by `manifest.type_id` (e.g. `"openrouter_credits"`) so
 *  every instance of a multi-instance extension shares one widget. */
class ExtensionWidgetRegistry {
  private widgets = new Map<string, ExtensionWidget>();

  register(typeId: string, widget: ExtensionWidget) {
    this.widgets.set(typeId, widget);
  }

  get(typeId: string): ExtensionWidget | undefined {
    return this.widgets.get(typeId);
  }

  clear() {
    this.widgets.clear();
  }
}

export const widgetRegistry = new ExtensionWidgetRegistry();

/** Register all bespoke widgets shipped with Hyvemind. Called once
 *  during app initialization (see `main.tsx` / `App.tsx`). */
export function registerWidgets(reg: ExtensionWidgetRegistry = widgetRegistry) {
  reg.register("chatgpt_sub_usage", ChatGptSubUsagePill);
  reg.register("claude_sub_usage", ClaudeSubUsagePill);
  reg.register("crof_usage", CrofUsagePill);
  reg.register("deepseek_balance", DeepseekBalancePill);
  reg.register("neokens_balance", NeokensBalancePill);
  reg.register("neuralwatt_usage", NeuralWattUsagePill);
  reg.register("openrouter_credits", OpenRouterCreditsPill);
  // Future bespoke widgets register here.
}

/** Single-pill sortable wrapper. The entire pill is the drag target —
 *  there is no separate handle — mirroring the "press-hold + slide"
 *  interaction described in the topbar reorder feature. */
function SortablePill({
  id,
  children,
}: {
  id: string;
  children: React.ReactNode;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.6 : undefined,
    zIndex: isDragging ? 50 : undefined,
    touchAction: "none",
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      {...listeners}
      {...attributes}
      className="cursor-grab active:cursor-grabbing"
    >
      {children}
    </div>
  );
}

/** Topbar slot — renders one pill per visible, ok-status extension.
 *  Filters: snapshot present, status === "ok", user_settings.show_in_topbar.
 *
 *  Pills are horizontally reorderable via drag-and-drop. The chosen
 *  order is persisted to localStorage; new extensions append at the
 *  end alphabetically, removed/disabled extensions disappear
 *  naturally. The DndContext is scoped to a wrapping <div> that
 *  contains *only* the pills — no other topbar element is wrapped
 *  and `restrictToParentElement` confines dragging to that wrapper. */
export function ExtensionTopbarSlot() {
  const { snapshots } = useExtensions();
  const [savedOrder, setSavedOrder] = useState<string[]>(() => loadOrder());

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    }),
  );

  const visible = snapshots.filter(
    (e) =>
      e.status === "ok" &&
      e.snapshot?.headline &&
      e.user_settings.show_in_topbar,
  );
  if (visible.length === 0) return null;

  const ordered = applyOrder(visible, savedOrder);
  const ids = ordered.map((e) => e.manifest.id);

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = ids.indexOf(String(active.id));
    const newIndex = ids.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(ids, oldIndex, newIndex);
    setSavedOrder(next);
    saveOrder(next);
  };

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      modifiers={[restrictToHorizontalAxis, restrictToParentElement]}
      onDragEnd={handleDragEnd}
    >
      <SortableContext items={ids} strategy={horizontalListSortingStrategy}>
        <div className="flex items-center gap-2">
          {ordered.map((entry) => {
            const Widget =
              widgetRegistry.get(entry.manifest.type_id) ?? DefaultUsagePill;
            return (
              <SortablePill key={entry.manifest.id} id={entry.manifest.id}>
                <Widget entry={entry} />
              </SortablePill>
            );
          })}
        </div>
      </SortableContext>
    </DndContext>
  );
}
