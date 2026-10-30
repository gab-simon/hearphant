UUID      := hearphant@hearphant.org
SRC       := $(UUID)
DBUS_XML  := ../../dbus/org.hearphant.Service1.xml
BUILD     := build

.PHONY: pack install uninstall test clean

pack: $(BUILD)/$(UUID).shell-extension.zip

$(BUILD)/$(UUID).shell-extension.zip: $(wildcard $(SRC)/*) $(wildcard $(SRC)/icons/*) $(wildcard $(SRC)/schemas/*) $(DBUS_XML)
	mkdir -p $(BUILD)
	gnome-extensions pack --force --out-dir=$(BUILD) \
		--extra-source=ellie.js --extra-source=service.js --extra-source=states.js \
		--extra-source=icons --extra-source=$(abspath $(DBUS_XML)) \
		--schema=schemas/org.gnome.shell.extensions.hearphant.gschema.xml \
		$(SRC)

install: pack
	gnome-extensions install --force $(BUILD)/$(UUID).shell-extension.zip
	@echo "Instalada. Em X11: Alt+F2, 'r'. Em Wayland: encerre a sessão. Depois: gnome-extensions enable $(UUID)"

uninstall:
	gnome-extensions uninstall $(UUID)

test:
	gjs tests/run.js

clean:
	rm -rf $(BUILD)
