/* Cliente do serviço org.hearphant.Service1. A extensão não calcula nada:
 * só observa o nome no barramento, lê o status e repassa sinais. */
'use strict';

const { Gio, GLib } = imports.gi;
const ByteArray = imports.byteArray;

const ExtensionUtils = imports.misc.extensionUtils;
const Me = ExtensionUtils.getCurrentExtension();

var BUS_NAME = 'org.hearphant.Service1';
var OBJECT_PATH = '/org/hearphant/Service1';

// Intervalo mínimo entre atualizações de UI (seção 14.4: sem polling por quadro).
const UPDATE_THROTTLE_MS = 250;

let _proxyClass = null;

function _getProxyClass() {
    if (!_proxyClass) {
        const file = Me.dir.get_child(`${BUS_NAME}.xml`);
        const [, contents] = file.load_contents(null);
        _proxyClass = Gio.DBusProxy.makeProxyWrapper(ByteArray.toString(contents));
    }
    return _proxyClass;
}

function _unpackDict(dict) {
    const out = {};
    for (const [key, value] of Object.entries(dict ?? {}))
        out[key] = value instanceof GLib.Variant ? value.recursiveUnpack() : value;
    return out;
}

var HearPhantClient = class HearPhantClient {
    /* callbacks: onStatus(rawOrNull), onAlert(alert), onProtection(result) */
    constructor(callbacks) {
        this._callbacks = callbacks;
        this._proxy = null;
        this._signalIds = [];
        this._cancellable = null;
        this._pendingStatus = undefined;
        this._throttleId = 0;

        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            () => this._onAppeared(),
            () => this._onVanished());
    }

    get connected() {
        return this._proxy !== null;
    }

    _call(method, ...args) {
        return new Promise((resolve, reject) => {
            if (!this._proxy) {
                reject(new Error('Serviço HearPhant indisponível'));
                return;
            }
            this._proxy[`${method}Remote`](...args, (result, error) => {
                if (error)
                    reject(error);
                else
                    resolve(result);
            });
        });
    }

    async setSignalAnalysis(enabled) {
        const [accepted, reason] = await this._call('SetSignalAnalysis', enabled);
        return { accepted, reason };
    }

    /* Nível atual do sinal; usar só enquanto um medidor estiver visível. */
    async getLevel() {
        const [capturing, rmsDbfs, peakDbfs, audible, laeq] = await this._call('GetLevel');
        return { capturing, rmsDbfs, peakDbfs, audible, laeqEstimate: laeq >= 0 ? laeq : null };
    }

    async setDeviceKind(kind) {
        const [accepted, reason] = await this._call('SetDeviceKind', kind);
        return { accepted, reason };
    }

    async deleteHistory(fromUtcMs, toUtcMs) {
        const [deleted] = await this._call('DeleteHistory', fromUtcMs, toUtcMs);
        return deleted;
    }

    async setProtectionMode(mode) {
        const [accepted, reason] = await this._call('SetProtectionMode', mode);
        return { accepted, reason };
    }

    _onAppeared() {
        this._dropProxy();
        this._cancellable = new Gio.Cancellable();
        const Proxy = _getProxyClass();
        new Proxy(Gio.DBus.session, BUS_NAME, OBJECT_PATH, (proxy, error) => {
            if (error) {
                if (!error.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED)) {
                    logError(error, 'HearPhant: falha ao criar proxy D-Bus');
                    this._queueStatus({ mascot_state: 'ERROR', measurement_status: 'UNAVAILABLE' });
                }
                return;
            }
            this._proxy = proxy;
            this._signalIds = [
                proxy.connectSignal('StatusChanged', (_p, _s, [status]) =>
                    this._queueStatus(_unpackDict(status))),
                proxy.connectSignal('AlertRaised', (_p, _s, [alert]) =>
                    this._callbacks.onAlert(_unpackDict(alert))),
                proxy.connectSignal('ProtectionApplied', (_p, _s, [result]) =>
                    this._callbacks.onProtection(_unpackDict(result))),
            ];
            this._refresh();
        }, this._cancellable, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
    }

    _onVanished() {
        this._dropProxy();
        this._queueStatus(null);
    }

    _refresh() {
        this._proxy?.GetStatusRemote((result, error) => {
            if (error) {
                logError(error, 'HearPhant: GetStatus falhou');
                this._queueStatus({ mascot_state: 'ERROR', measurement_status: 'UNAVAILABLE' });
                return;
            }
            this._queueStatus(_unpackDict(result[0]));
        });
    }

    _queueStatus(raw) {
        this._pendingStatus = raw;
        if (this._throttleId)
            return;
        this._flushStatus();
        this._throttleId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, UPDATE_THROTTLE_MS, () => {
            this._throttleId = 0;
            if (this._pendingStatus !== undefined)
                this._flushStatus();
            return GLib.SOURCE_REMOVE;
        });
    }

    _flushStatus() {
        const raw = this._pendingStatus;
        this._pendingStatus = undefined;
        this._callbacks.onStatus(raw);
    }

    _dropProxy() {
        this._cancellable?.cancel();
        this._cancellable = null;
        if (this._proxy) {
            for (const id of this._signalIds)
                this._proxy.disconnectSignal(id);
        }
        this._signalIds = [];
        this._proxy = null;
    }

    destroy() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        if (this._throttleId) {
            GLib.source_remove(this._throttleId);
            this._throttleId = 0;
        }
        this._dropProxy();
        this._callbacks = { onStatus() {}, onAlert() {}, onProtection() {} };
    }
};
