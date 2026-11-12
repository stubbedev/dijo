Name:           lighthouse-log
Version:        1.2
Release:        3
Summary:        The keeper's log of the lighthouse
License:        CC0-1.0
URL:            https://example.org/lighthouse
Packager:       The Keeper <keeper@example.org>
Vendor:         Lighthouse Press
Group:          Documentation
BuildArch:      noarch

%description
The lighthouse keeper's log: what the lamp needed each night
and how the storms passed the tower.

%install
mkdir -p %{buildroot}/usr/share/lighthouse/1890s
printf 'Every evening at dusk the keeper climbed the tower and lit the lamp.\n' > %{buildroot}/usr/share/lighthouse/ReadMe.txt
printf 'Storm from the north-west; the lamp burned until dawn.\n' > %{buildroot}/usr/share/lighthouse/1890s/storm.txt

%files
/usr/share/lighthouse

%changelog
* Thu Nov 12 2026 The Keeper <keeper@example.org> - 1.2-3
- Record the storm of the north-west.
