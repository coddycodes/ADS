import { Empty, EmptyHeader, EmptyTitle } from "./ui/empty";
import { Spinner } from "./ui/spinner";
import { SidebarInset } from "./ui/sidebar";
import { cn } from "~/lib/utils";
import { COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS } from "~/workspaceTitlebar";

export function NoActiveThreadState() {
  return (
    <SidebarInset className="h-dvh min-h-0 overflow-hidden overscroll-y-none bg-background text-foreground">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-x-hidden bg-background">
        <header
          className={cn(
            "border-b border-panel-separator px-3 transition-[padding-left] duration-200 ease-linear motion-reduce:transition-none sm:px-5",
            "workspace-topbar",
            COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS,
          )}
        >
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-foreground md:text-muted-foreground/60">
              No active thread
            </span>
          </div>
        </header>

        <Empty className="flex-1">
          <div className="w-full max-w-lg px-8 py-12">
            <EmptyHeader className="max-w-none">
              <EmptyTitle className="text-foreground text-xl">Let's CoddyStudio...</EmptyTitle>
            </EmptyHeader>
          </div>
        </Empty>
      </div>
    </SidebarInset>
  );
}

/** A chat panel's tab exists before the server confirms its thread; show progress, not "no thread". */
export function ChatPanelOpeningState() {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center gap-2 bg-background text-sm text-muted-foreground">
      <Spinner className="size-4" aria-hidden />
      Opening chat…
    </div>
  );
}
