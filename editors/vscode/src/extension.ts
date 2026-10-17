import * as vscode from 'vscode';
import * as cp from 'child_process';

export function activate(context: vscode.ExtensionContext) {
    const formatter = vscode.languages.registerDocumentFormattingEditProvider('nexum', {
        provideDocumentFormattingEdits(document) {
            const nx = vscode.workspace.getConfiguration('nexum').get<string>('nxPath', 'nx');
            try {
                const out = cp.execFileSync(nx, ['fmt', document.fileName], { encoding: 'utf8' });
                const fullRange = new vscode.Range(
                    document.positionAt(0),
                    document.positionAt(document.getText().length)
                );
                return [vscode.TextEdit.replace(fullRange, out)];
            } catch (e) {
                vscode.window.showErrorMessage(`Nexum format failed: ${e}`);
                return [];
            }
        }
    });
    context.subscriptions.push(formatter);

    const cmd = vscode.commands.registerCommand('nexum.formatDocument', () => {
        vscode.commands.executeCommand('editor.action.formatDocument');
    });
    context.subscriptions.push(cmd);

    // Format on save.
    context.subscriptions.push(
        vscode.workspace.onWillSaveTextDocument((e) => {
            const cfg = vscode.workspace.getConfiguration('nexum');
            if (cfg.get<boolean>('formatOnSave', true)) {
                e.waitUntil(vscode.commands.executeCommand('editor.action.formatDocument').then(() => [] as vscode.TextEdit[]));
            }
        })
    );
}

export function deactivate() {}