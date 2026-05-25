import { createContext, useContext, useState } from 'react';

interface StackContextValue {
  stack: string;
  setStack: (s: string) => void;
  editing: boolean;
  setEditing: (e: boolean) => void;
}

const StackContext = createContext<StackContextValue>({
  stack: 'my-stack',
  setStack: () => {},
  editing: false,
  setEditing: () => {},
});

const STORAGE_KEY = 'nullnet_stack';

export function StackProvider({ children }: { children: React.ReactNode }) {
  const [stack, setStackState] = useState(() => localStorage.getItem(STORAGE_KEY) ?? 'my-stack');
  const [editing, setEditing] = useState(false);

  function setStack(s: string) {
    setStackState(s);
    localStorage.setItem(STORAGE_KEY, s);
  }

  return (
    <StackContext.Provider value={{ stack, setStack, editing, setEditing }}>
      {children}
    </StackContext.Provider>
  );
}

export function useStack() {
  return useContext(StackContext);
}
