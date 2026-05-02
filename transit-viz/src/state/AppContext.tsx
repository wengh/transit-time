import { createContext, useContext, useReducer, ReactNode, Dispatch } from 'react';
import { reducer, initialState, type AppState, type Action } from './reducer';

interface ContextValue {
  state: AppState;
  dispatch: Dispatch<Action>;
}

const AppContext = createContext<ContextValue | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  return <AppContext.Provider value={{ state, dispatch }}>{children}</AppContext.Provider>;
}

export function useAppState(): ContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useAppState must be used within AppProvider');
  return ctx;
}
