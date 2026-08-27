public class n2
{
	private List<m7> m_d;

	public void ai()
	{
		int num4 = 0;
		while (num4 < 6)
		{
			this.m_d.Add(new m7());
			this.m_d[num4].OffsetProgrammableMemoryAddress = 8192 * num4;
			num4++;
		}
	}

	public void a6(n7 A_0)
	{
		int num4 = 0;
		while (num4 < 6)
		{
			this.m_d[num4].a6(A_0);
			num4++;
		}
	}

	public void a7(n7 A_0)
	{
		this.m_d[0].a7(A_0);
	}
}
