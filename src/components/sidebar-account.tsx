import { ChevronsUpDown, LogOut, RefreshCw, UserRound } from 'lucide-react';
import { DropdownMenu } from 'radix-ui';
import { Button } from '@/components/ui/button';

type SidebarAccountProps = {
  account:{name:string}|null;
  disabled:boolean;
  onSignIn:()=>void;
  onSignOut:()=>void;
};

export function SidebarAccount({account,disabled,onSignIn,onSignOut}:SidebarAccountProps) {
  return <div className="sidebar-account-area">
    {account?<DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <Button variant="ghost" className="sidebar-account" disabled={disabled} aria-label={`Account menu for ${account.name}`}>
          <span className="account-avatar"><UserRound size={15}/></span>
          <span className="sidebar-account-name" title={account.name}>{account.name}</span>
          <ChevronsUpDown size={14} className="sidebar-account-chevron"/>
        </Button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="account-menu" side="top" align="start" sideOffset={8} collisionPadding={12} aria-label="Account actions">
          <DropdownMenu.Item className="account-menu-item" disabled={disabled} onSelect={onSignIn}><RefreshCw size={15}/>Change account</DropdownMenu.Item>
          <DropdownMenu.Separator className="account-menu-separator"/>
          <DropdownMenu.Item className="account-menu-item" disabled={disabled} onSelect={onSignOut}><LogOut size={15}/>Sign out</DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>:<Button variant="outline" className="sidebar-sign-in" disabled={disabled} onClick={onSignIn}><UserRound size={15}/>Sign in to Microsoft</Button>}
  </div>;
}
