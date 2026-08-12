import type { HTMLAttributes, ReactNode } from "react";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator
} from "@/components/ui/breadcrumb";
import { cn } from "@/lib/cn";

/** 提供所有业务页一致的纵向节奏与最小宽度约束。 */
export function Page({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("flex min-w-0 flex-col gap-6", className)} {...props} />;
}

/** 承载页面标题、说明与右侧主要动作。 */
export function PageHeader({ className, ...props }: HTMLAttributes<HTMLElement>) {
  return (
    <header
      className={cn("flex min-w-0 flex-col gap-4 sm:flex-row sm:items-end sm:justify-between", className)}
      {...props}
    />
  );
}

/** 渲染页面主标题与可选说明。 */
export function PageHeading({
  title,
  description,
  eyebrow,
  breadcrumb,
  className
}: {
  title: ReactNode;
  description?: ReactNode;
  eyebrow?: ReactNode;
  breadcrumb?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("min-w-0", className)}>
      {breadcrumb && <PageBreadcrumb current={breadcrumb} />}
      {eyebrow && <div className="mb-1 text-xs font-semibold text-primary">{eyebrow}</div>}
      <h1 className="text-2xl font-bold leading-8 tracking-normal">{title}</h1>
      {description && <p className="mt-1 max-w-3xl text-sm leading-5 text-muted-foreground">{description}</p>}
    </div>
  );
}

/** 按 Stitch 顶部路径规范展示产品根节点与当前页面。 */
export function PageBreadcrumb({ current }: { current: ReactNode }) {
  return (
    <Breadcrumb className="mb-1.5 hidden sm:block">
      <BreadcrumbList className="gap-1.5 text-xs">
        <BreadcrumbItem>
          <span>Ani-tracker</span>
        </BreadcrumbItem>
        <BreadcrumbSeparator />
        <BreadcrumbItem>
          <BreadcrumbPage className="font-semibold text-primary">{current}</BreadcrumbPage>
        </BreadcrumbItem>
      </BreadcrumbList>
    </Breadcrumb>
  );
}

/** 在页面标题区排列主要与次要动作。 */
export function PageActions({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("flex w-full min-w-0 flex-wrap items-center gap-2 sm:w-auto sm:justify-end", className)}
      {...props}
    />
  );
}

/** 以无卡片嵌套的边界带展示页面关键指标。 */
export function MetricStrip({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "grid min-w-0 grid-cols-2 gap-px overflow-hidden rounded-md border bg-border",
        className
      )}
      {...props}
    />
  );
}

/** 渲染摘要带中的单项标签、数值与补充信息。 */
export function MetricItem({
  label,
  value,
  detail,
  className
}: {
  label: ReactNode;
  value: ReactNode;
  detail?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("min-w-0 bg-card px-4 py-3", className)}>
      <div className="truncate text-xs font-medium text-muted-foreground">{label}</div>
      <div className="mt-1 truncate text-xl font-semibold tabular-nums">{value}</div>
      {detail && <div className="mt-1 truncate text-xs text-muted-foreground">{detail}</div>}
    </div>
  );
}

/** 承载搜索、筛选、排序与视图切换控件。 */
export function FilterToolbar({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "flex min-w-0 flex-col gap-3 border-b border-border/60 bg-card/50 py-3 sm:flex-row sm:items-center sm:justify-between",
        className
      )}
      {...props}
    />
  );
}

/** 展示页面或服务的即时状态，不额外创建装饰卡片。 */
export function StatusRow({
  icon,
  title,
  detail,
  actions,
  className
}: {
  icon?: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex min-w-0 items-center gap-3 border-b py-3", className)}>
      {icon && <div className="shrink-0 text-muted-foreground">{icon}</div>}
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium">{title}</div>
        {detail && <div className="mt-0.5 truncate text-xs text-muted-foreground">{detail}</div>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </div>
  );
}

/** 在页面底部固定保存、取消等关键动作，并保留安全区。 */
export function StickyActionBar({ className, ...props }: HTMLAttributes<HTMLElement>) {
  return (
    <footer
      className={cn(
        "fixed bottom-0 left-0 right-0 z-40 flex min-w-0 flex-wrap items-center justify-end gap-2 border-t bg-background px-4 py-3 pb-[max(0.75rem,var(--safe-area-bottom))] md:left-[4.5rem] md:px-5 xl:left-[14rem] xl:px-6",
        className
      )}
      {...props}
    />
  );
}
