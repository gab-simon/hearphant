/* HearPhant — indicador da Ellie para GNOME Shell 42.
 * O GNOME 42 não tem a API de Quick Settings (43+), então a integração usa
 * um botão de painel com popover de três telas: principal, semana e
 * configurações. Toda a regra de negócio fica no serviço
 * (org.hearphant.Service1); aqui só há apresentação. */
'use strict';

const { Clutter, GLib, GObject, Graphene, Pango, St } = imports.gi;

const Main = imports.ui.main;
const MessageTray = imports.ui.messageTray;
const PanelMenu = imports.ui.panelMenu;
const PopupMenu = imports.ui.popupMenu;

const ExtensionUtils = imports.misc.extensionUtils;
const Me = ExtensionUtils.getCurrentExtension();
const { EllieRing, WeekChart, LevelMeter, ShareBar, ellieIcon, animationsEnabled, setAnimationsSetting } =
    Me.imports.ellie;
const { HearPhantClient } = Me.imports.service;
const States = Me.imports.states;

const HOVER_TILT_DEGREES = -10;
const HOVER_TILT_MS = 150;
const LEVEL_POLL_MS = 250;
const LEVEL_POLL_ESTIMATE_MS = 1000;
const TOOLTIP_DELAY_MS = 400;
const DELETE_CONFIRM_MS = 4000;

function _label(text, styleClass, props = {}) {
    const label = new St.Label({ text, style_class: styleClass, ...props });
    label.clutter_text.line_wrap = true;
    label.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
    return label;
}

function _setLine(label, text) {
    label.visible = text !== null && text !== '';
    if (label.visible)
        label.text = text;
}

/* Linha clicável no estilo do mockup: ícone · título/subtítulo · acessório. */
function _row({ icon, title, subtitle = null, trailing = null, onClick = null, toggle = false }) {
    const content = new St.BoxLayout({ style_class: 'hearphant-row-content', x_expand: true });
    const iconBin = new St.Bin({ style_class: 'hearphant-row-icon', y_align: Clutter.ActorAlign.CENTER });
    const iconActor = new St.Icon({ icon_name: icon, icon_size: 18 });
    iconBin.set_child(iconActor);
    content.add_child(iconBin);

    // Título e subtítulo numa linha cada, com reticências: texto quebrado
    // dentro de botões tem altura mal calculada no St e corta o conteúdo.
    // Explicações longas vão em notas abaixo do cartão (ver `_section`).
    const texts = new St.BoxLayout({ vertical: true, x_expand: true, y_align: Clutter.ActorAlign.CENTER });
    const titleLabel = new St.Label({ text: title, style_class: 'hearphant-row-title' });
    const subtitleLabel = new St.Label({ text: subtitle ?? '', style_class: 'hearphant-row-subtitle' });
    for (const l of [titleLabel, subtitleLabel])
        l.clutter_text.ellipsize = Pango.EllipsizeMode.END;
    subtitleLabel.visible = subtitle !== null;
    texts.add_child(titleLabel);
    texts.add_child(subtitleLabel);
    content.add_child(texts);

    if (trailing) {
        trailing.y_align = Clutter.ActorAlign.CENTER;
        content.add_child(trailing);
    }

    const actor = onClick
        ? new St.Button({ child: content, style_class: 'hearphant-row', can_focus: true, x_expand: true, toggle_mode: toggle })
        : new St.Bin({ child: content, style_class: 'hearphant-row static', x_expand: true });
    if (onClick)
        actor.connect('clicked', onClick);
    actor.accessible_name = subtitle ? `${title}. ${subtitle}` : title;
    return { actor, icon: iconActor, title: titleLabel, subtitle: subtitleLabel };
}

function _chevron() {
    return new St.Icon({ icon_name: 'go-next-symbolic', icon_size: 14, style_class: 'hearphant-chevron' });
}

function _section(title, noteText = null) {
    const box = new St.BoxLayout({ vertical: true, style_class: 'hearphant-section', x_expand: true });
    box.add_child(_label(title, 'hearphant-section-title'));
    const card = new St.BoxLayout({ vertical: true, style_class: 'hearphant-card hearphant-list', x_expand: true });
    box.add_child(card);
    const note = _label(noteText ?? '', 'hearphant-section-note');
    note.visible = noteText !== null;
    box.add_child(note);
    return { box, card, note };
}

function _tile(icon, caption) {
    const tile = new St.BoxLayout({ style_class: 'hearphant-card hearphant-tile', x_expand: true });
    const iconActor = new St.Icon({ icon_name: icon, icon_size: 20, style_class: 'hearphant-tile-icon', y_align: Clutter.ActorAlign.START });
    tile.add_child(iconActor);
    const texts = new St.BoxLayout({ vertical: true, x_expand: true });
    texts.add_child(_label(caption, 'hearphant-tile-caption'));
    const value = _label('—', 'hearphant-tile-value');
    const note = _label('', 'hearphant-tile-note');
    texts.add_child(value);
    texts.add_child(note);
    tile.add_child(texts);
    return { actor: tile, icon: iconActor, value, note };
}

function _pageHeader(title, onBack) {
    const header = new St.BoxLayout({ style_class: 'hearphant-page-header', x_expand: true });
    const back = new St.Button({
        child: new St.Icon({ icon_name: 'go-previous-symbolic', icon_size: 16 }),
        style_class: 'hearphant-icon-button',
        can_focus: true,
        accessible_name: 'Voltar',
    });
    back.connect('clicked', onBack);
    header.add_child(back);
    header.add_child(new St.Label({
        text: title,
        style_class: 'hearphant-page-title',
        x_expand: true,
        y_align: Clutter.ActorAlign.CENTER,
    }));
    return header;
}

const HearPhantIndicator = GObject.registerClass(
class HearPhantIndicator extends PanelMenu.Button {
    _init(settings) {
        super._init(0.5, 'HearPhant');

        this._settings = settings;
        this._status = States.unavailableStatus();
        this._notificationSource = null;
        this._levelPollId = 0;
        this._levelInFlight = false;
        this._tooltipId = 0;
        this._deleteArmedId = 0;
        this._currentPage = 'main';

        this._panelIcon = new St.Icon({
            gicon: ellieIcon(),
            style_class: 'system-status-icon hearphant-panel-icon',
            pivot_point: new Graphene.Point({ x: 0.5, y: 0.5 }),
        });
        this.add_child(this._panelIcon);
        this.connect('notify::hover', () => this._onHover());

        this._tooltip = new St.Label({ style_class: 'hearphant-tooltip', visible: false });
        this._tooltip.clutter_text.line_wrap = true;
        Main.uiGroup.add_child(this._tooltip);

        this._buildMenu();
        this.menu.connect('open-state-changed', (_m, open) => this._onMenuOpen(open));

        this._client = new HearPhantClient({
            onStatus: raw => this._setStatus(States.normalizeStatus(raw)),
            onAlert: alert => this._onAlert(alert),
            onProtection: result => this._onProtection(result),
        });

        this._setStatus(this._status);
    }

    /* ---------- construção ---------- */

    _buildMenu() {
        this.menu.box.add_style_class_name('hearphant-menu');
        // `activate: false` mantém o item reativo (sem o estado :insensitive,
        // que acinzenta o texto) mas sem fechar o menu ao clicar.
        const item = new PopupMenu.PopupBaseMenuItem({
            activate: false,
            can_focus: false,
            style_class: 'hearphant-menu-item',
        });
        item.remove_style_class_name('popup-inactive-menu-item');
        // A coluna do ornamento (marcador de seleção) desloca tudo à direita.
        item._ornamentLabel.hide();
        this._pages = {
            main: this._buildMainPage(),
            week: this._buildWeekPage(),
            settings: this._buildSettingsPage(),
        };
        const stack = new St.Widget({ layout_manager: new Clutter.BinLayout(), x_expand: true });
        for (const page of Object.values(this._pages))
            stack.add_child(page);
        item.add_child(stack);
        this.menu.addMenuItem(item);
        this._showPage('main');
    }

    _newPage() {
        return new St.BoxLayout({ vertical: true, x_expand: true, style_class: 'hearphant-page' });
    }

    _buildMainPage() {
        const page = this._newPage();

        const header = new St.BoxLayout({ style_class: 'hearphant-header', x_expand: true });
        const logo = new St.Bin({ style_class: 'hearphant-logo', y_align: Clutter.ActorAlign.CENTER });
        logo.set_child(new St.Icon({ gicon: ellieIcon(), icon_size: 22 }));
        header.add_child(logo);
        const titles = new St.BoxLayout({ vertical: true, x_expand: true, y_align: Clutter.ActorAlign.CENTER });
        titles.add_child(new St.Label({ text: 'HearPhant', style_class: 'hearphant-title' }));
        titles.add_child(new St.Label({ text: 'A Ellie acompanha sua escuta', style_class: 'hearphant-subtitle' }));
        header.add_child(titles);
        const gear = new St.Button({
            child: new St.Icon({ icon_name: 'emblem-system-symbolic', icon_size: 16 }),
            style_class: 'hearphant-icon-button',
            can_focus: true,
            accessible_name: 'Configurações',
            y_align: Clutter.ActorAlign.CENTER,
        });
        gear.connect('clicked', () => this._showPage('settings'));
        header.add_child(gear);
        page.add_child(header);

        this._demoBadge = new St.Label({
            text: 'DEMONSTRAÇÃO · dados fictícios',
            style_class: 'hearphant-demo-badge',
            x_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        page.add_child(this._demoBadge);

        this._ring = new EllieRing();
        page.add_child(this._ring);

        this._hero = new St.Label({ style_class: 'hearphant-hero', x_align: Clutter.ActorAlign.CENTER });
        this._heroCaption = _label('', 'hearphant-hero-caption', { x_align: Clutter.ActorAlign.CENTER });
        this._mood = _label('', 'hearphant-mood', { x_align: Clutter.ActorAlign.CENTER });
        this._doseChipLabel = new St.Label({ style_class: 'hearphant-chip-label' });
        this._doseChip = new St.Button({
            child: this._doseChipLabel,
            style_class: 'hearphant-chip',
            x_align: Clutter.ActorAlign.CENTER,
            can_focus: true,
        });
        this._doseChip.connect('clicked', () => this._showPage('settings'));
        page.add_child(this._hero);
        page.add_child(this._heroCaption);
        page.add_child(this._mood);
        page.add_child(this._doseChip);

        const tiles = new St.BoxLayout({ style_class: 'hearphant-tiles', x_expand: true });
        this._mainTiles = tiles;
        this._volumeTile = _tile('audio-volume-high-symbolic', 'Volume atual');
        this._deviceTile = _tile('audio-card-symbolic', 'Dispositivo');
        tiles.add_child(this._volumeTile.actor);
        tiles.add_child(this._deviceTile.actor);
        page.add_child(tiles);

        // Medidor de sinal ao vivo.
        const signalCard = new St.BoxLayout({ vertical: true, style_class: 'hearphant-card hearphant-signal', x_expand: true });
        const signalTop = new St.BoxLayout({ x_expand: true });
        this._signalCaption = new St.Label({ text: 'Sinal agora', style_class: 'hearphant-tile-caption', x_expand: true });
        signalTop.add_child(this._signalCaption);
        this._signalValue = new St.Label({ text: '—', style_class: 'hearphant-signal-value' });
        signalTop.add_child(this._signalValue);
        signalCard.add_child(signalTop);
        this._meter = new LevelMeter();
        signalCard.add_child(this._meter);
        this._meterScale = new St.BoxLayout({ style_class: 'hearphant-meter-scale', x_expand: true });
        this._meterScale.add_child(new St.Label({ text: '40', x_expand: true, style_class: 'hearphant-tile-note' }));
        this._meterScale.add_child(new St.Label({ text: 'marcas em 80 e 90 · 110 dB(A)', style_class: 'hearphant-tile-note' }));
        signalCard.add_child(this._meterScale);
        this._signalHint = _label('', 'hearphant-tile-note');
        signalCard.add_child(this._signalHint);
        this._signalCard = signalCard;
        page.add_child(signalCard);

        this._protectionRow = _row({
            icon: 'security-medium-symbolic',
            title: 'Proteção',
            subtitle: '',
            trailing: _chevron(),
            onClick: () => this._showPage('settings'),
        });
        this._protectionRow.actor.add_style_class_name('hearphant-card');
        page.add_child(this._protectionRow.actor);

        const details = new St.Button({
            style_class: 'hearphant-primary-button',
            can_focus: true,
            x_expand: true,
            label: 'Ver escuta da semana  →',
        });
        details.connect('clicked', () => this._showPage('week'));
        this._detailsButton = details;
        page.add_child(details);

        this._footer = _label('', 'hearphant-footnote', { x_align: Clutter.ActorAlign.CENTER });
        page.add_child(this._footer);
        return page;
    }

    _buildWeekPage() {
        const page = this._newPage();
        page.add_child(_pageHeader('Escuta da semana', () => this._showPage('main')));

        const chartCard = new St.BoxLayout({ vertical: true, style_class: 'hearphant-card hearphant-chart-card', x_expand: true });
        this._chart = new WeekChart();
        chartCard.add_child(this._chart);
        page.add_child(chartCard);

        this._doseTile = _tile('emblem-important-symbolic', 'Exposição estimada');
        page.add_child(this._doseTile.actor);

        const stats = new St.BoxLayout({ style_class: 'hearphant-tiles', x_expand: true });
        this._totalTile = _tile('preferences-system-time-symbolic', 'Tempo com áudio');
        this._avgTile = _tile('view-continuous-symbolic', 'Média diária');
        stats.add_child(this._totalTile.actor);
        stats.add_child(this._avgTile.actor);
        page.add_child(stats);

        this._meanTile = _tile('audio-volume-medium-symbolic', 'Sinal médio');
        page.add_child(this._meanTile.actor);

        page.add_child(_label('Detalhamento por dispositivo', 'hearphant-section-title'));
        this._deviceList = new St.BoxLayout({ vertical: true, style_class: 'hearphant-card hearphant-list', x_expand: true });
        page.add_child(this._deviceList);

        this._sourceNote = _label('', 'hearphant-footnote');
        this._coverageNote = _label('', 'hearphant-footnote');
        page.add_child(this._sourceNote);
        page.add_child(this._coverageNote);
        return page;
    }

    _switchRow(icon, title, subtitle, onToggle) {
        const sw = new PopupMenu.Switch(false);
        const row = _row({ icon, title, subtitle, trailing: sw, toggle: true, onClick: () => onToggle(!sw.state) });
        return { ...row, sw };
    }

    _buildSettingsPage() {
        const page = this._newPage();
        page.add_child(_pageHeader('Configurações', () => this._showPage('main')));

        const scroll = new St.ScrollView({
            style_class: 'hearphant-settings-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            x_expand: true,
            y_expand: true,
        });
        const body = new St.BoxLayout({ vertical: true, x_expand: true, style_class: 'hearphant-settings-body' });
        scroll.add_actor(body);
        page.add_child(scroll);

        const general = _section('Geral',
            'A análise mede o som que sai do computador. Enquanto ela roda, o GNOME mostra o ícone de gravação; ' +
            'o microfone não é usado e nada é gravado. Volume alto, excessos e falhas de proteção sempre notificam.');
        this._signalSwitch = this._switchRow('audio-input-microphone-symbolic', 'Análise do nível do sinal',
            'Necessária para estimar a exposição',
            enabled => this._requestSignalAnalysis(enabled));
        this._notifSwitch = this._switchRow('preferences-system-notifications-symbolic', 'Notificações',
            'Alertas informativos',
            v => this._settings.set_boolean('notifications', v));
        this._animSwitch = this._switchRow('media-playback-start-symbolic', 'Animações da Ellie',
            'Movimento da orelha e do anel',
            v => this._settings.set_boolean('animations', v));
        for (const r of [this._signalSwitch, this._notifSwitch, this._animSwitch])
            general.card.add_child(r.actor);
        body.add_child(general.box);

        const protection = _section('Proteção');
        this._modeRows = new Map();
        for (const mode of States.ProtectionModes) {
            const check = new St.Icon({ icon_name: 'object-select-symbolic', icon_size: 16, style_class: 'hearphant-check' });
            const row = _row({
                icon: 'security-medium-symbolic',
                title: mode.label,
                subtitle: mode.hint,
                trailing: check,
                onClick: () => this._requestProtectionMode(mode),
            });
            this._modeRows.set(mode.id, { ...row, check });
            protection.card.add_child(row.actor);
        }
        body.add_child(protection.box);

        const device = _section('Dispositivo de áudio', '');
        this._calibrationNote = device.note;
        this._settingsDevice = _row({ icon: 'audio-card-symbolic', title: 'Dispositivo principal', subtitle: '—' });
        this._settingsCalibration = _row({ icon: 'emblem-default-symbolic', title: 'Estimativa de exposição', subtitle: '—' });
        device.card.add_child(this._settingsDevice.actor);
        device.card.add_child(this._settingsCalibration.actor);
        body.add_child(device.box);

        const kind = _section('Tipo desta saída');
        this._kindRows = new Map();
        for (const choice of States.KIND_CHOICES) {
            const check = new St.Icon({ icon_name: 'object-select-symbolic', icon_size: 16, style_class: 'hearphant-check' });
            const row = _row({
                icon: choice.id === 'speaker' ? 'audio-speakers-symbolic'
                    : choice.id === 'headphone' ? 'audio-headphones-symbolic' : 'find-location-symbolic',
                title: choice.label,
                trailing: check,
                onClick: () => this._requestDeviceKind(choice),
            });
            this._kindRows.set(choice.id, { ...row, check });
            kind.card.add_child(row.actor);
        }
        body.add_child(kind.box);

        const reference = _section('Referência');
        reference.card.add_child(_row({
            icon: 'preferences-system-time-symbolic',
            title: 'Modelo de referência',
            subtitle: 'OMS/UIT adulto · 80 dB(A) × 40 h/semana',
        }).actor);
        body.add_child(reference.box);

        const data = _section('Dados');
        this._deleteRow = _row({
            icon: 'user-trash-symbolic',
            title: 'Apagar histórico',
            subtitle: 'Remove o histórico deste computador',
            onClick: () => this._onDeleteClicked(),
        });
        data.card.add_child(this._deleteRow.actor);
        body.add_child(data.box);

        body.add_child(_label('HearPhant não é dispositivo médico. Sem calibração medida, a exposição é uma estimativa com faixa de incerteza.', 'hearphant-footnote'));
        return page;
    }

    _showPage(name) {
        this._currentPage = name;
        for (const [key, page] of Object.entries(this._pages))
            page.visible = key === name;
        this._syncLevelPolling();
    }

    /* ---------- estado ---------- */

    _setStatus(status) {
        this._status = status;
        const view = States.describe(status);
        const week = States.describeWeek(status);
        const connected = this._client?.connected ?? false;

        for (const cls of Object.values(States.MascotState))
            this._panelIcon.remove_style_class_name(`hearphant-state-${cls.toLowerCase()}`);
        this._panelIcon.add_style_class_name(view.styleClass);
        for (const actor of [this._panelIcon, this._ring]) {
            if (view.loud)
                actor.add_style_class_name('hearphant-loud');
            else
                actor.remove_style_class_name('hearphant-loud');
        }
        if (view.loud && !this._wasLoud)
            this._wiggle();
        this._wasLoud = view.loud;
        // Ícone esmaecido sem serviço: nada tranquilizador quando não sabemos (14.2).
        this._panelIcon.opacity = status.mascotState === States.MascotState.UNAVAILABLE ? 128 : 255;
        this.accessible_name = view.accessibleName;
        this._tooltipText = `${view.hero === '—' ? view.heroCaption : `${view.hero} ${view.heroCaption}`}\n${view.mood}`;
        if (this._tooltip.visible)
            this._tooltip.text = `HearPhant\n${this._tooltipText}`;

        // Principal.
        this._demoBadge.visible = status.isDemo;
        this._ring.setState(view.styleClass, view.ringFraction, view.showQuestionBadge);
        this._ring.accessible_name = view.accessibleName;
        this._hero.text = view.hero;
        this._heroCaption.text = view.heroCaption;
        this._mood.text = view.mood;
        this._doseChip.visible = view.doseChip !== null;
        this._doseChipLabel.text = view.doseChip ?? '';
        this._doseChip.reactive = view.doseChipAction;
        this._doseChip.can_focus = view.doseChipAction;
        if (view.doseChipAction)
            this._doseChip.add_style_class_name('action');
        else
            this._doseChip.remove_style_class_name('action');
        this._volumeTile.value.text = view.volumeTile;
        this._volumeTile.note.text = view.volumeTileCaption;
        this._deviceTile.value.text = view.deviceTile;
        this._deviceTile.note.text = view.deviceTileCaption;
        this._deviceTile.icon.icon_name = view.deviceIcon;
        const available = status.mascotState !== States.MascotState.UNAVAILABLE;
        this._signalCard.visible = available;
        this._mainTiles.visible = available;
        this._detailsButton.visible = available;
        this._signalHint.text = view.signal.hint;
        if (view.signal.state !== 'live') {
            this._signalValue.text = view.signal.label;
            this._meter.setLevel(null, null);
        }
        this._liveEstimate = view.signal.state === 'live' && status.estimateAvailable;
        this._estimateUncertaintyDb = status.estimateUncertaintyDb;
        this._signalCaption.text = this._liveEstimate ? 'Agora no ouvido (estimativa)' : 'Sinal agora';
        this._meter.setScale(this._liveEstimate ? States.LIVE_SCALE : null);
        this._meterScale.visible = this._liveEstimate;
        this._protectionRow.actor.visible = connected;
        this._protectionRow.title.text = view.protectionLabel ? `Proteção: ${view.protectionLabel}` : 'Proteção';
        this._protectionRow.subtitle.text = view.calibrated
            ? 'Alertas e redução de volume'
            : 'Sem calibração, a Ellie só informa';
        _setLine(this._footer, view.coverageLine);

        // Semana.
        this._doseTile.actor.visible = view.estimated || view.calibrated;
        this._doseTile.value.text = `${view.hero} da referência`;
        this._doseTile.note.text = view.estimated
            ? `${view.doseChip}. Estimativa automática, não é medição.`
            : view.sourceLine;
        this._chart.setBars(week.bars);
        this._totalTile.value.text = week.total;
        this._totalTile.note.text = week.totalCaption;
        this._avgTile.value.text = week.dailyAverage;
        this._avgTile.note.text = week.dailyAverageCaption;
        this._meanTile.value.text = week.meanSignal;
        this._meanTile.note.text = week.meanSignalCaption;
        this._deviceList.destroy_all_children();
        if (week.devices.length === 0)
            this._deviceList.add_child(_label('Nenhum áudio registrado ainda.', 'hearphant-row-subtitle hearphant-empty'));
        for (const d of week.devices) {
            const row = new St.BoxLayout({ style_class: 'hearphant-device-row', x_expand: true });
            row.add_child(new St.Icon({ icon_name: d.icon, icon_size: 18, style_class: 'hearphant-row-icon', y_align: Clutter.ActorAlign.CENTER }));
            const texts = new St.BoxLayout({ vertical: true, x_expand: true });
            const top = new St.BoxLayout({ x_expand: true });
            top.add_child(new St.Label({ text: d.name, style_class: 'hearphant-row-title', x_expand: true }));
            top.add_child(new St.Label({ text: `${d.percentLabel} · ${d.durationLabel}`, style_class: 'hearphant-row-subtitle' }));
            texts.add_child(top);
            texts.add_child(new ShareBar(d.fraction));
            row.add_child(texts);
            row.accessible_name = `${d.name}: ${d.percentLabel}, ${d.durationLabel}`;
            this._deviceList.add_child(row);
        }
        this._sourceNote.text = week.sourceNote;
        this._coverageNote.text = week.coverage;

        // Configurações.
        this._setSwitch(this._signalSwitch, status.signalAnalysis);
        this._signalSwitch.actor.reactive = connected;
        this._setSwitch(this._notifSwitch, this._settings.get_boolean('notifications'));
        this._setSwitch(this._animSwitch, this._settings.get_boolean('animations'));
        for (const [id, row] of this._modeRows) {
            row.check.opacity = id === status.protectionMode ? 255 : 0;
            row.actor.reactive = connected;
            row.actor.checked = id === status.protectionMode;
        }
        this._settingsDevice.icon.icon_name = view.deviceIcon;
        this._settingsDevice.subtitle.text = status.deviceName
            ? `${status.deviceName} · ${States.DEVICE_KINDS[status.deviceKind]}`
            : 'Nenhuma saída detectada';
        this._settingsCalibration.subtitle.text = view.estimateShort;
        this._calibrationNote.text = view.calibrationLine;
        this._calibrationNote.visible = true;
        for (const [id, row] of this._kindRows) {
            row.check.opacity = id === status.kindOverride ? 255 : 0;
            row.actor.reactive = connected && !!status.deviceName;
            row.actor.checked = id === status.kindOverride;
        }
        this._deleteRow.actor.reactive = connected;

        this._syncLevelPolling();
    }

    _setSwitch(row, state) {
        row.sw.state = state;
        row.actor.checked = state;
    }

    /* ---------- medidor ao vivo ---------- */

    _onMenuOpen(open) {
        this._hideTooltip();
        if (open)
            this._showPage('main');
        else
            this._disarmDelete();
        this._syncLevelPolling();
    }

    _syncLevelPolling() {
        const want = this.menu.isOpen && this._currentPage === 'main' &&
            this._status.captureStatus === 'capturing' && (this._client?.connected ?? false);
        // Com estimativa, o valor muda a cada bloco de 3 s: 1 consulta/s basta.
        const interval = this._liveEstimate ? LEVEL_POLL_ESTIMATE_MS : LEVEL_POLL_MS;
        if (want && this._levelPollId && this._levelPollInterval !== interval) {
            GLib.source_remove(this._levelPollId);
            this._levelPollId = 0;
        }
        if (want && !this._levelPollId) {
            this._pollLevel();
            this._levelPollInterval = interval;
            this._levelPollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, interval, () => {
                this._pollLevel();
                return GLib.SOURCE_CONTINUE;
            });
        } else if (!want && this._levelPollId) {
            GLib.source_remove(this._levelPollId);
            this._levelPollId = 0;
        }
    }

    async _pollLevel() {
        if (this._levelInFlight || !this._client)
            return;
        this._levelInFlight = true;
        try {
            const level = await this._client.getLevel();
            if (!this._levelPollId && !this.menu.isOpen)
                return;
            if (level.capturing && this._liveEstimate) {
                // Modo estimativa: a barra segue o volume (dB(A) no ouvido).
                const live = States.describeLive(level.laeqEstimate, this._estimateUncertaintyDb);
                this._meter.setLevel(live ? level.laeqEstimate : null);
                this._setMeterZone(live?.zone ?? null);
                this._signalValue.text = live ? live.label : 'Silêncio';
                const dbfs = level.audible ? `Sinal ${States.formatDbfs(level.rmsDbfs)}. ` : '';
                this._signalHint.text = live ? `${dbfs}${live.hint}` : 'Sem som saindo agora.';
            } else if (level.capturing) {
                this._meter.setLevel(level.rmsDbfs, level.peakDbfs);
                this._setMeterZone(null);
                this._signalValue.text = level.audible ? States.formatDbfs(level.rmsDbfs) : 'Silêncio';
            } else {
                this._meter.setLevel(null, null);
                this._signalValue.text = 'Aguardando';
            }
        } catch (e) {
            this._signalValue.text = '—';
        } finally {
            this._levelInFlight = false;
        }
    }

    _setMeterZone(zone) {
        for (const z of ['ok', 'high', 'very-high'])
            this._meter.remove_style_class_name(`zone-${z}`);
        if (zone)
            this._meter.add_style_class_name(`zone-${zone}`);
    }

    /* ---------- ações ---------- */

    async _requestProtectionMode(mode) {
        try {
            const { accepted, reason } = await this._client.setProtectionMode(mode.id);
            if (!accepted)
                this._notify('Modo de proteção não aplicado', reason || `O serviço recusou o modo “${mode.label}”.`, { always: true });
        } catch (e) {
            this._notify('Modo de proteção não aplicado', e.message, { always: true });
        }
    }

    async _requestSignalAnalysis(enabled) {
        // Mostra a escolha na hora; o estado real chega por StatusChanged.
        this._setSwitch(this._signalSwitch, enabled);
        let failure = null;
        try {
            const { accepted, reason } = await this._client.setSignalAnalysis(enabled);
            if (!accepted)
                failure = reason || 'O serviço recusou a mudança.';
        } catch (e) {
            failure = e.message;
        }
        if (failure !== null) {
            this._setSwitch(this._signalSwitch, this._status.signalAnalysis);
            this._notify('Análise do sinal', failure, { always: true });
        }
    }

    async _requestDeviceKind(choice) {
        try {
            const { accepted, reason } = await this._client.setDeviceKind(choice.id);
            if (!accepted)
                this._notify('Tipo da saída', reason || 'O serviço recusou a mudança.', { always: true });
        } catch (e) {
            this._notify('Tipo da saída', e.message, { always: true });
        }
    }

    _onDeleteClicked() {
        if (!this._deleteArmedId) {
            this._deleteRow.title.text = 'Toque de novo para apagar';
            this._deleteRow.actor.add_style_class_name('danger');
            this._deleteArmedId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, DELETE_CONFIRM_MS, () => {
                this._deleteArmedId = 0;
                this._disarmDelete();
                return GLib.SOURCE_REMOVE;
            });
            return;
        }
        this._disarmDelete();
        this._client.deleteHistory(0, Number.MAX_SAFE_INTEGER)
            .then(n => this._notify('Histórico apagado', `${n} ${n === 1 ? 'registro removido' : 'registros removidos'} deste computador.`, { always: true }))
            .catch(e => this._notify('Não foi possível apagar o histórico', e.message, { always: true }));
    }

    _disarmDelete() {
        if (this._deleteArmedId) {
            GLib.source_remove(this._deleteArmedId);
            this._deleteArmedId = 0;
        }
        this._deleteRow.title.text = 'Apagar histórico';
        this._deleteRow.actor.remove_style_class_name('danger');
    }

    /* ---------- painel ---------- */

    _onHover() {
        if (!this.hover) {
            this._hideTooltip();
            return;
        }
        if (!this.menu.isOpen && !this._tooltipId) {
            this._tooltipId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, TOOLTIP_DELAY_MS, () => {
                this._tooltipId = 0;
                this._showTooltip();
                return GLib.SOURCE_REMOVE;
            });
        }
        if (!animationsEnabled())
            return;
        // Inclinação discreta da orelha ao passar o mouse; volta sozinha.
        this._panelIcon.remove_all_transitions();
        this._panelIcon.ease({
            rotation_angle_z: HOVER_TILT_DEGREES,
            duration: HOVER_TILT_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
            onComplete: () => this._panelIcon.ease({
                rotation_angle_z: 0,
                duration: HOVER_TILT_MS,
                mode: Clutter.AnimationMode.EASE_IN_QUAD,
            }),
        });
    }

    /* A Ellie "chama atenção" uma vez quando o volume entra na zona vermelha. */
    _wiggle() {
        if (!animationsEnabled())
            return;
        const icon = this._panelIcon;
        icon.remove_all_transitions();
        const swing = (angles, i = 0) => {
            if (i >= angles.length)
                return;
            icon.ease({
                rotation_angle_z: angles[i],
                duration: 90,
                mode: Clutter.AnimationMode.EASE_IN_OUT_QUAD,
                onComplete: () => swing(angles, i + 1),
            });
        };
        swing([-14, 14, -10, 10, 0]);
    }

    _showTooltip() {
        if (!this.hover || this.menu.isOpen)
            return;
        this._tooltip.text = `HearPhant\n${this._tooltipText}`;
        this._tooltip.show();
        const [x, y] = this.get_transformed_position();
        const [w, h] = this.get_transformed_size();
        const [, tipW] = this._tooltip.get_preferred_width(-1);
        const monitor = Main.layoutManager.findMonitorForActor(this);
        const minX = monitor ? monitor.x + 8 : 8;
        const maxX = monitor ? monitor.x + monitor.width - tipW - 8 : x;
        this._tooltip.set_position(Math.round(Math.min(Math.max(x + w / 2 - tipW / 2, minX), maxX)), Math.round(y + h + 6));
    }

    _hideTooltip() {
        if (this._tooltipId) {
            GLib.source_remove(this._tooltipId);
            this._tooltipId = 0;
        }
        this._tooltip.hide();
    }

    /* ---------- notificações ---------- */

    _onAlert(alert) {
        const title = typeof alert.title === 'string' ? alert.title : 'HearPhant';
        const body = typeof alert.body === 'string' ? alert.body : '';
        // Volume alto e referência excedida sempre avisam (pedido explícito).
        const critical = alert.severity === 'exceeded' || alert.severity === 'loud';
        this._notify(title, body, { urgent: alert.severity === 'exceeded', always: critical });
    }

    _onProtection(result) {
        // Só confirma redução quando o serviço verificou o ganho aplicado (seção 12.3).
        if (result.verified === true) {
            this._notify('Volume reduzido',
                result.message || 'A Ellie reduziu o volume deste dispositivo. Confira se ele ficou confortável.',
                { always: true });
        } else {
            this._notify('Proteção não confirmada',
                result.message || 'Não consegui controlar esta saída de áudio. Ajuste o volume diretamente no equipamento.',
                { urgent: true, always: true });
        }
    }

    /* `always`: proteção, erros e excessos não podem ser silenciados (12.4). */
    _notify(title, body, { urgent = false, always = false } = {}) {
        if (!always && !this._settings.get_boolean('notifications'))
            return;
        if (!this._notificationSource) {
            this._notificationSource = new MessageTray.Source('HearPhant', null);
            this._notificationSource.createIcon = size => new St.Icon({ gicon: ellieIcon(), icon_size: size });
            this._notificationSource.connect('destroy', () => {
                this._notificationSource = null;
            });
            Main.messageTray.add(this._notificationSource);
        }
        const notification = new MessageTray.Notification(this._notificationSource, title, body);
        notification.setTransient(!urgent);
        notification.setUrgency(urgent ? MessageTray.Urgency.HIGH : MessageTray.Urgency.NORMAL);
        this._notificationSource.showNotification(notification);
    }

    destroy() {
        this._hideTooltip();
        this._disarmDelete();
        if (this._levelPollId) {
            GLib.source_remove(this._levelPollId);
            this._levelPollId = 0;
        }
        this._tooltip.destroy();
        this._client?.destroy();
        this._client = null;
        this._notificationSource?.destroy();
        super.destroy();
    }
});

class Extension {
    enable() {
        this._settings = ExtensionUtils.getSettings();
        setAnimationsSetting(() => this._settings?.get_boolean('animations') ?? true);
        this._indicator = new HearPhantIndicator(this._settings);
        this._settingsId = this._settings.connect('changed', () => this._indicator?._setStatus(this._indicator._status));
        Main.panel.addToStatusArea(Me.uuid, this._indicator);
    }

    disable() {
        if (this._settingsId) {
            this._settings.disconnect(this._settingsId);
            this._settingsId = 0;
        }
        this._indicator?.destroy();
        this._indicator = null;
        setAnimationsSetting(() => true);
        this._settings = null;
    }
}

function init() {
    return new Extension();
}
