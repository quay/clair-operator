all: $(patsubst %.gv,%.png,$(wildcard *.gv))

%.png: %.gv
	dot -Tpng <$< >$@_
	pngcrush -q $@_ $@
	rm $@_

clean:
	rm -f *.png

.PHONY: clean
