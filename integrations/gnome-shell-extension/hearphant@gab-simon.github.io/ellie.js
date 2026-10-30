/* Ellie + anel de progresso para o popover (seção 6.5). */
'use strict';

const Cairo = imports.cairo;
const { Clutter, Gio, GLib, GObject, St } = imports.gi;

const ExtensionUtils = imports.misc.extensionUtils;
const Me = ExtensionUtils.getCurrentExtension();

const RING_TRANSITION_MS = 400;
const FRAME_MS = 16;

function ellieIcon() {
    return new Gio.FileIcon({ file: Me.dir.get_child('icons').get_child('ellie-symbolic.svg') });
}

/* Animações respeitam o sistema e a preferência da extensão. */
let _animationsSetting = () => true;

function setAnimationsSetting(getter) {
    _animationsSetting = getter;
}

function animationsEnabled() {
    return St.Settings.get().enable_animations && _animationsSetting();
}

function _rgb(widget) {
    const c = widget.get_theme_node().get_foreground_color();
    return [c.red / 255, c.green / 255, c.blue / 255];
}

var EllieRing = GObject.registerClass(
class EllieRing extends St.Widget {
    _init() {
        super._init({
            layout_manager: new Clutter.BinLayout(),
            style_class: 'hearphant-ellie-ring',
            x_align: Clutter.ActorAlign.CENTER,
        });

        // fraction === null desenha anel tracejado: "dose desconhecida", nunca 0%.
        this._fraction = null;
        this._shownFraction = null;
        this._animationId = 0;

        this._area = new St.DrawingArea({ style_class: 'hearphant-ring-area', x_expand: true, y_expand: true });
        this._area.connect('repaint', area => this._repaint(area));
        this.add_child(this._area);

        this._halo = new St.Widget({
            style_class: 'hearphant-ellie-halo',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this.add_child(this._halo);

        this._icon = new St.Icon({
            gicon: ellieIcon(),
            style_class: 'hearphant-ellie-large',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this.add_child(this._icon);

        this._badge = new St.Label({
            text: '?',
            style_class: 'hearphant-ellie-badge',
            x_align: Clutter.ActorAlign.END,
            y_align: Clutter.ActorAlign.START,
            visible: false,
        });
        this.add_child(this._badge);

        this.connect('destroy', () => this._stopAnimation());
    }

    setState(styleClass, fraction, showQuestionBadge) {
        if (this._stateClass)
            this.remove_style_class_name(this._stateClass);
        this._stateClass = styleClass;
        this.add_style_class_name(styleClass);
        this._badge.visible = showQuestionBadge;
        this._setFraction(fraction);
    }

    _setFraction(fraction) {
        const from = this._shownFraction;
        this._fraction = fraction;
        this._stopAnimation();

        if (fraction === null || from === null || !animationsEnabled() || !this.mapped) {
            this._shownFraction = fraction;
            this._area.queue_repaint();
            return;
        }

        // Transição suave e curta; sem animação contínua (seção 5.5).
        const start = GLib.get_monotonic_time();
        this._animationId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, FRAME_MS, () => {
            const t = Math.min((GLib.get_monotonic_time() - start) / 1000 / RING_TRANSITION_MS, 1);
            const eased = 1 - (1 - t) ** 3;
            this._shownFraction = from + (fraction - from) * eased;
            this._area.queue_repaint();
            if (t < 1)
                return GLib.SOURCE_CONTINUE;
            this._animationId = 0;
            return GLib.SOURCE_REMOVE;
        });
    }

    _stopAnimation() {
        if (this._animationId) {
            GLib.source_remove(this._animationId);
            this._animationId = 0;
        }
    }

    _repaint(area) {
        const cr = area.get_context();
        const [width, height] = area.get_surface_size();
        const [r, g, b] = _rgb(area);

        const lineWidth = Math.max(6, Math.min(width, height) * 0.075);
        const radius = Math.min(width, height) / 2 - lineWidth / 2 - 1;
        const cx = width / 2;
        const cy = height / 2;
        // Abertura em baixo, como no mockup: arco de 300°.
        const gap = Math.PI / 3;
        const start = Math.PI / 2 + gap / 2;
        const sweep = 2 * Math.PI - gap;

        cr.setLineWidth(lineWidth);
        cr.setLineCap(Cairo.LineCap.ROUND);

        if (this._shownFraction === null) {
            cr.setSourceRGBA(r, g, b, 0.45);
            cr.setDash([lineWidth * 0.2, lineWidth * 1.6], 0);
            cr.arc(cx, cy, radius, start, start + sweep);
            cr.stroke();
        } else {
            cr.setSourceRGBA(r, g, b, 0.16);
            cr.arc(cx, cy, radius, start, start + sweep);
            cr.stroke();

            // O anel enche até 100%; o excedente aparece no texto, não truncado.
            const fill = Math.min(this._shownFraction, 1);
            if (fill > 0) {
                cr.setSourceRGBA(r, g, b, 1);
                cr.arc(cx, cy, radius, start, start + sweep * fill);
                cr.stroke();
            }
        }
        cr.$dispose();
    }
});

/* Barras dos 7 dias. Altura relativa ao maior dia, com piso de 30 min para
 * dias curtos não parecerem enormes. */
var WeekChart = GObject.registerClass(
class WeekChart extends St.BoxLayout {
    _init() {
        super._init({ vertical: true, style_class: 'hearphant-week-chart', x_expand: true });
        this._bars = [];

        this._values = new St.BoxLayout({ style_class: 'hearphant-week-values', x_expand: true });
        this.add_child(this._values);

        this._area = new St.DrawingArea({ style_class: 'hearphant-week-area', x_expand: true });
        this._area.connect('repaint', area => this._repaint(area));
        this.add_child(this._area);

        this._labels = new St.BoxLayout({ style_class: 'hearphant-week-labels', x_expand: true });
        this.add_child(this._labels);
    }

    setBars(bars) {
        this._bars = bars;
        for (const box of [this._values, this._labels])
            box.destroy_all_children();
        for (const bar of bars) {
            this._values.add_child(new St.Label({
                text: bar.valueLabel,
                x_expand: true,
                style_class: `hearphant-week-value${bar.isToday ? ' today' : ''}`,
            }));
            const label = new St.Label({
                text: bar.label,
                x_expand: true,
                style_class: `hearphant-week-label${bar.isToday ? ' today' : ''}`,
            });
            label.accessible_name = bar.accessible;
            this._labels.add_child(label);
        }
        this._area.queue_repaint();
    }

    _repaint(area) {
        const cr = area.get_context();
        const [width, height] = area.get_surface_size();
        const [r, g, b] = _rgb(area);
        const n = this._bars.length || 7;
        const slot = width / n;
        const barWidth = Math.min(slot * 0.5, 22);
        const max = Math.max(30 * 60000, ...this._bars.map(x => x.ms));

        // Linha de base.
        cr.setSourceRGBA(r, g, b, 0.15);
        cr.rectangle(0, height - 1, width, 1);
        cr.fill();

        this._bars.forEach((bar, i) => {
            const x = slot * i + (slot - barWidth) / 2;
            if (bar.noData) {
                // Sem dados ≠ zero: contorno tracejado, sem preenchimento.
                const h = Math.min(18, height - 2);
                cr.save();
                cr.setSourceRGBA(r, g, b, 0.35);
                cr.setLineWidth(1);
                cr.setDash([3, 3], 0);
                cr.rectangle(x + 0.5, height - h + 0.5, barWidth - 1, h - 1);
                cr.stroke();
                cr.restore();
                return;
            }
            const h = bar.ms > 0 ? Math.max(4, (height - 4) * bar.ms / max) : 2;
            const y = height - h;
            const radius = Math.min(barWidth / 2, 6, h / 2);
            cr.setSourceRGBA(r, g, b, bar.isToday ? 1 : 0.45);
            cr.newSubPath();
            cr.arc(x + barWidth - radius, y + radius, radius, -Math.PI / 2, 0);
            cr.lineTo(x + barWidth, height);
            cr.lineTo(x, height);
            cr.arc(x + radius, y + radius, radius, Math.PI, 1.5 * Math.PI);
            cr.closePath();
            cr.fill();
        });
        cr.$dispose();
    }
});

/* Medidor horizontal de nível. Escala padrão: dBFS de −80 a 0; com
 * estimativa, dB(A) de 40 a 110 com marcas de referência. */
const DBFS_SCALE = Object.freeze({ min: -80, max: 0, markers: [] });

var LevelMeter = GObject.registerClass(
class LevelMeter extends St.DrawingArea {
    _init() {
        super._init({ style_class: 'hearphant-level-meter', x_expand: true });
        this._value = null;
        this._peak = null;
        this._scale = DBFS_SCALE;
        this.connect('repaint', area => this._repaint(area));
    }

    setScale(scale) {
        this._scale = scale ?? DBFS_SCALE;
        this.queue_repaint();
    }

    setLevel(value, peak = null) {
        this._value = value;
        this._peak = peak;
        this.queue_repaint();
    }

    _repaint(area) {
        const cr = area.get_context();
        const [width, height] = area.get_surface_size();
        const [r, g, b] = _rgb(area);
        const { min, max, markers } = this._scale;
        const pos = v => Math.min(Math.max((v - min) / (max - min), 0), 1) * width;
        const radius = height / 2;

        const pill = w => {
            cr.newSubPath();
            cr.arc(radius, radius, radius, Math.PI / 2, 1.5 * Math.PI);
            cr.arc(Math.max(w - radius, radius), radius, radius, -Math.PI / 2, Math.PI / 2);
            cr.closePath();
        };

        cr.setSourceRGBA(r, g, b, 0.15);
        pill(width);
        cr.fill();

        if (this._value !== null && this._value > min) {
            cr.setSourceRGBA(r, g, b, 0.9);
            pill(Math.max(pos(this._value), height));
            cr.fill();
        }
        if (this._peak !== null && this._peak > min) {
            cr.setSourceRGBA(r, g, b, 1);
            cr.rectangle(Math.min(pos(this._peak), width - 2), 0, 2, height);
            cr.fill();
        }
        // Marcas de referência (ex.: 80 e 90 dB(A)) em cinza neutro.
        cr.setSourceRGBA(0.5, 0.5, 0.5, 0.8);
        for (const m of markers) {
            cr.rectangle(Math.round(pos(m)) - 1, 0, 2, height);
            cr.fill();
        }
        cr.$dispose();
    }
});

/* Barra fina de proporção (divisão por dispositivo). */
var ShareBar = GObject.registerClass(
class ShareBar extends St.DrawingArea {
    _init(fraction) {
        super._init({ style_class: 'hearphant-share-bar', x_expand: true });
        this._fraction = fraction;
        this.connect('repaint', area => {
            const cr = area.get_context();
            const [width, height] = area.get_surface_size();
            const [r, g, b] = _rgb(area);
            cr.setLineCap(Cairo.LineCap.ROUND);
            cr.setLineWidth(height);
            cr.setSourceRGBA(r, g, b, 0.15);
            cr.moveTo(height / 2, height / 2);
            cr.lineTo(width - height / 2, height / 2);
            cr.stroke();
            if (this._fraction > 0) {
                cr.setSourceRGBA(r, g, b, 1);
                cr.moveTo(height / 2, height / 2);
                cr.lineTo(height / 2 + (width - height) * Math.min(this._fraction, 1), height / 2);
                cr.stroke();
            }
            cr.$dispose();
        });
    }
});
